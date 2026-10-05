#![cfg(test)]

#[path = "../../../benchmarks/lww-native/src/fixture.rs"]
mod fixture;
#[path = "../../../benchmarks/lww-native/src/native_observation.rs"]
mod native_observation;
#[path = "../../../benchmarks/lww-native/src/invalid_diagnostics.rs"]
mod invalid_diagnostics;
#[path = "../../../benchmarks/lww-native/src/native_shards.rs"]
mod native_shards;
#[path = "../../../benchmarks/lww-native/src/scale_certificate.rs"]
mod scale_certificate;
#[allow(dead_code)]
#[path = "../../../benchmarks/lww-native/src/measurement.rs"]
mod measurement;
#[path = "../../../benchmarks/lww-native/src/final_runner.rs"]
mod final_runner;
#[allow(dead_code)]
#[path = "../../../benchmarks/lww-native/src/costly_runner.rs"]
mod costly_runner;
#[path = "../../../benchmarks/lww-native/src/fixture_configuration.rs"]
mod fixture_configuration;
#[allow(dead_code)]
#[path = "../../../benchmarks/lww-native/src/native_bootstrap.rs"]
mod native_bootstrap;
#[path = "../../../benchmarks/lww-native/src/native_maintenance.rs"]
#[allow(dead_code)]
mod native_maintenance;
#[allow(dead_code)]
#[path = "../../../benchmarks/lww-native/src/source_receipt.rs"]
mod source_receipt;
#[allow(dead_code)]
#[path = "../../../benchmarks/lww-native/src/source_process.rs"]
mod source_process;
#[path = "../../../benchmarks/lww-native/src/final_matrix.rs"]
mod final_matrix;
#[path = "../../../benchmarks/lww-native/src/ladder.rs"]
mod ladder;

use super::{lww::{self, MessageLocator, UnitMutation}, sync_selection::{SwitchBindingRequest, SyncTarget},
    ConversationMutation, PersistentStore, WorkingSetCommit};
use crate::{external_storage::{contract::Cancellation, lww_tests::CycleFixture, lww_segment},
    server_sync::{self, lww_client::LwwClient, lww_tests::LocalServerFixture}};
use fixture::{FixtureReceipt, FixtureScale};
use native_observation::{BodyDomain, NativeObservation, ObjectBodyDomain};
use risunest_sync_wire::unit::UnitKey;
use serde::Serialize;
use serde_json::{json, Value};
use std::{fs::File, path::Path, sync::Arc, time::Instant};

pub(crate) fn final_device_file_costly_slot(
    slot:&costly_runner::CostlySlot,store:PersistentStore,source:Option<crate::native_file_jobs::OpenedJobSource>,
    owned:std::path::PathBuf,handoffs:&Path,jobs:Arc<crate::native_file_jobs::NativeFileJobState>,
    persistent:Arc<crate::persistent_store::commands::PersistentStoreState>,device:Arc<crate::device_backup::DeviceBackupState>,
    asset_hashes:Vec<String>,capture_ready:impl Fn(&str,i64)+Send+Sync+'static,
) ->Result<Value,String> {
    if slot.warmup!=(slot.iteration<costly_runner::WARMUPS) || slot.iteration>=costly_runner::WARMUPS+costly_runner::REPETITIONS {
        return Err("device-file costly slot differs from the fixed matrix".into());
    }
    let mut raw=match slot.scenario {
        measurement::Scenario::DuringBackup=>native_maintenance::portable_backup_raw(
            store,&owned,handoffs,&asset_hashes,slot.iteration,capture_ready)?,
        measurement::Scenario::RestoreAllPresent|measurement::Scenario::RestoreMissing=>native_maintenance::portable_restore_raw(
            source.ok_or("actual portable source handle is required")?,store,owned,jobs,persistent,device,asset_hashes,
            slot.iteration,slot.scenario==measurement::Scenario::RestoreAllPresent)?,
        _=>return Err("scenario does not use the device-file backup route".into()),
    };
    raw["direction"]=serde_json::to_value(slot.direction).map_err(|e|e.to_string())?;
    Ok(raw)
}

/// The actual capture callback holds the coherent backup lease until the foreground cycle joins.
pub(crate) fn final_backup_overlap_costly_slot(
    slot:&costly_runner::CostlySlot,backup:PersistentStore,owned:&Path,handoffs:&Path,
    foreground:PersistentStore,peer:PersistentStore,sender:LwwClient,receiver:LwwClient,
    asset_hashes:Vec<String>,
) ->Result<Value,String> {
    if slot.scenario!=measurement::Scenario::DuringBackup || slot.warmup!=(slot.iteration<costly_runner::WARMUPS)
        || slot.iteration>=costly_runner::WARMUPS+costly_runner::REPETITIONS {
        return Err("backup overlap slot differs from the fixed matrix".into());
    }
    if backup.repository_root()!=foreground.repository_root() || foreground.repository_root()==peer.repository_root() {
        return Err("backup overlap requires the actual library and an independent peer".into());
    }
    let inputs=Arc::new(std::sync::Mutex::new(Some((foreground,peer,sender,receiver,asset_hashes.clone()))));
    let result=Arc::new(std::sync::Mutex::new(None));
    let result_in_callback=result.clone();
    let direction=slot.direction;let iteration=slot.iteration;let warmup=slot.warmup;
    let mut raw=native_maintenance::portable_backup_raw(backup,owned,handoffs,&asset_hashes,iteration,move |lease,revision| {
        let mut output=result_in_callback.lock().unwrap();
        let Some((mut foreground,mut peer,sender,receiver,assets))=inputs.lock().unwrap().take() else {
            *output=Some((lease.to_owned(),revision,Err(final_runner::CycleFailure {
                reason:"backup capture callback repeated".into(),observation:None})));return;
        };
        let worker=std::thread::spawn(move ||final_server_cycle(&mut foreground,&mut peer,&sender,&receiver,&assets,
            measurement::Scenario::SettingEdit,direction,iteration,warmup));
        let observed=match worker.join() {Ok(observed)=>observed,Err(_)=>Err(final_runner::CycleFailure {
            reason:"backup foreground worker panicked".into(),observation:None})};
        *output=Some((lease.to_owned(),revision,observed));
    })?;
    match result.lock().unwrap().take() {
        Some((lease,revision,Ok(sample)))=>{
            raw["foregroundObservation"]=serde_json::to_value(sample).map_err(|e|e.to_string())?;
            raw["foregroundCaptureLease"]=json!({"lease":lease,"revision":revision,"workerJoinedWhileLeaseHeld":true});
        },
        Some((lease,revision,Err(failure)))=>{
            raw["foregroundObservation"]=json!({"status":"INVALID","reason":failure.reason,"nativeObservation":failure.observation});
            raw["foregroundCaptureLease"]=json!({"lease":lease,"revision":revision,"workerJoinedWhileLeaseHeld":true});
        },
        None=>raw["foregroundObservation"]=json!({"status":"INVALID","reason":"actual backup capture callback was not observed"}),
    }
    raw["direction"]=serde_json::to_value(direction).map_err(|e|e.to_string())?;
    Ok(raw)
}

pub(crate) fn final_snapshot_costly_slot(
    slot:&costly_runner::CostlySlot,store:&mut PersistentStore,snapshot_id:&str,request_id:&str,
    jobs:&crate::native_file_jobs::NativeFileJobState,scratch:&Path,asset_hashes:&[String],
    native_activated:impl FnOnce(i64)->Result<(),String>,
) ->Result<Value,String> {
    if !matches!(slot.scenario,measurement::Scenario::RestoreAllPresent|measurement::Scenario::RestoreMissing)
        || slot.warmup!=(slot.iteration<costly_runner::WARMUPS) || slot.iteration>=costly_runner::WARMUPS+costly_runner::REPETITIONS {
        return Err("snapshot costly slot differs from the fixed matrix".into());
    }
    let mut raw=native_maintenance::snapshot_restore_raw(store,snapshot_id,request_id,jobs,scratch,asset_hashes,
        slot.iteration,slot.scenario==measurement::Scenario::RestoreAllPresent,native_activated)?;
    raw["direction"]=serde_json::to_value(slot.direction).map_err(|e|e.to_string())?;
    Ok(raw)
}

pub(crate) fn final_server_bootstrap_costly_slot(
    slot:&costly_runner::CostlySlot,source:source_process::SourceProcess,destination:&mut PersistentStore,
    io:Arc<server_sync::client::TestIoCounters>,roles:&std::collections::BTreeMap<String,std::collections::BTreeSet<String>>,
    selected_character_id:Option<&str>,
) ->Result<Value,String> {
    if !matches!(slot.scenario,measurement::Scenario::FullBootstrap|measurement::Scenario::RestoreAllPresent|measurement::Scenario::RestoreMissing)
        || slot.warmup!=(slot.iteration<costly_runner::WARMUPS) || slot.iteration>=costly_runner::WARMUPS+costly_runner::REPETITIONS {
        return Err("server first-binding costly slot differs from the fixed matrix".into());
    }
    let mut record=native_bootstrap::server_bootstrap_source_raw(source,destination,io,roles,selected_character_id,slot.direction,slot.iteration)?;
    record.scenario=slot.scenario;
    let mut raw=serde_json::to_value(record).map_err(|e|e.to_string())?;
    raw["route"]=json!("server-sync-first-binding");
    Ok(raw)
}

pub(crate) fn final_server_transfer_costly_slot(
    slot:&costly_runner::CostlySlot,source:source_process::SourceProcess,
    destination:&mut PersistentStore,sender:&LwwClient,peer:&mut PersistentStore,receiver:&LwwClient,
    roles:&std::collections::BTreeMap<String,std::collections::BTreeSet<String>>,barrier_hash:&str,selected_character_id:Option<&str>,
) ->Result<Value,String> {
    if slot.scenario!=measurement::Scenario::DuringAssetTransfer || slot.warmup!=(slot.iteration<costly_runner::WARMUPS)
        || slot.iteration>=costly_runner::WARMUPS+costly_runner::REPETITIONS {
        return Err("asset-transfer costly slot differs from the fixed matrix".into());
    }
    serde_json::to_value(native_bootstrap::server_asset_transfer_overlap_raw(source,destination,sender,peer,receiver,roles,
        barrier_hash,selected_character_id,slot.direction,slot.iteration)?).map_err(|e|e.to_string())
}

pub(crate) async fn final_external_bootstrap_costly_slot(
    slot:&costly_runner::CostlySlot,store:&mut PersistentStore,
    engine:&crate::external_storage::lww_engine::ExternalLwwEngine,
    connected:Arc<crate::external_storage::connection_commands::ConnectedRepository>,
    provider:&Arc<crate::external_storage::fake::FakeProvider>,asset_hashes:&[String],selected_character_id:Option<&str>,
) ->Result<Value,String> {
    if slot.warmup!=(slot.iteration<costly_runner::WARMUPS) {return Err("external bootstrap warmup differs from the fixed matrix".into());}
    let mut raw=native_maintenance::external_bootstrap_raw(store,engine,connected,provider,asset_hashes,selected_character_id,slot.scenario,slot.iteration).await?;
    raw["direction"]=serde_json::to_value(slot.direction).map_err(|e|e.to_string())?;Ok(raw)
}

/// The compactor owns its runtime and counter scope; the foreground keeps the caller's scope.
pub(crate) async fn final_compaction_overlap_costly_slot(
    slot:&costly_runner::CostlySlot,compactor:crate::external_storage::lww_engine::ExternalLwwEngine,
    provider:Arc<crate::external_storage::fake::FakeProvider>,directory:std::path::PathBuf,job_id:String,writer:String,
    owner:crate::external_storage::leases::LeaseOwner,
    ledger:Arc<crate::external_storage::lease_ledger::LocalLeaseLedger>,
    source:&mut PersistentStore,destination:&mut PersistentStore,
    sender:&mut crate::external_storage::lww_engine::ExternalLwwEngine,
    receiver:&mut crate::external_storage::lww_engine::ExternalLwwEngine,asset_hashes:&[String],
) ->Result<Value,String> {
    use crate::external_storage::{leases,lww_compaction::{CompactionBarrier,install_compaction_barrier}};
    if slot.scenario!=measurement::Scenario::DuringCompaction || slot.warmup!=(slot.iteration<costly_runner::WARMUPS)
        || slot.iteration>=costly_runner::WARMUPS+costly_runner::REPETITIONS || !compactor.capabilities.lease_operations {
        return Err("compaction overlap requires the fixed slot and actual protection support".into());
    }
    if Arc::as_ptr(&sender.provider) as *const ()!=Arc::as_ptr(&provider) as *const ()
        || Arc::as_ptr(&receiver.provider) as *const ()!=Arc::as_ptr(&provider) as *const () {
        return Err("foreground compaction observer belongs to a different provider".into());
    }
    let barrier=CompactionBarrier::new();install_compaction_barrier(&job_id,barrier.clone());
    let (send,mut completed)=tokio::sync::oneshot::channel();
    let assets=asset_hashes.to_vec();let iteration=slot.iteration;let foreground_provider=provider.clone();
    let mut versions=source.open_native_job_store().map_err(|e|e.to_string())?;
    let background=std::thread::spawn(move || {
        let outcome=(||->Result<native_maintenance::RawCompaction,String> {
            let runtime=tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e|e.to_string())?;
            runtime.block_on(async {
                let context=leases::LeaseContext {root:&compactor.connection_root,connection_id:&compactor.connection_id,
                    writer_id:&writer,descriptor:&compactor.descriptor,root_key:&compactor.root_key,
                    provider:compactor.provider.as_ref(),repository:&compactor.repository,clock:leases::system_clock(),
                    protection_supported:compactor.capabilities.lease_operations,ledger:Some(ledger)};
                native_maintenance::compact_native_raw(&compactor,&provider,&mut versions,&directory,&job_id,&writer,&compactor.capabilities,
                    Some((&owner,&context)),&assets,measurement::Scenario::DuringCompaction,iteration).await
            })
        })();
        let _=send.send(outcome);
    });
    let mut foreground=None;let mut reached=false;
    let outcome=tokio::select! {
        result=&mut completed=>result.map_err(|_|"compaction worker ended without a receipt".to_owned()),
        _=barrier.reached.notified()=>{
            reached=true;
            foreground=Some(final_external_cycle(source,destination,sender,receiver,&foreground_provider,asset_hashes,
                measurement::Scenario::SettingEdit,slot.direction,slot.iteration,slot.warmup).await);
            barrier.resume.notify_one();
            completed.await.map_err(|_|"compaction worker ended without a receipt".to_owned())
        },
    };
    let joined=background.join();
    let mut errors=vec![];
    let background=match outcome {Ok(Ok(raw))=>Some(serde_json::to_value(raw).map_err(|e|e.to_string())?),
        Ok(Err(error))|Err(error)=>{errors.push(error);None}};
    if joined.is_err() {errors.push("compaction worker panicked".into());}
    let foreground=match foreground {Some(Ok(sample))=>Some(serde_json::to_value(sample).map_err(|e|e.to_string())?),
        Some(Err(failed))=>{errors.push(failed.reason.clone());Some(json!({"status":"INVALID","reason":failed.reason,
            "nativeObservation":failed.observation}))},None=>None};
    Ok(json!({"status":"INVALID","scenario":"during-compaction","direction":slot.direction,
        "iteration":slot.iteration,"warmup":slot.warmup,"actualProtectedInputBarrierReached":reached,
        "background":background,"foregroundObservation":foreground,
        "providerIoScope":"whole overlap, includes foreground; foreground provider counters are non-additive",
        "nativeCounterScopes":"separate actual compactor and foreground worker threads",
        "sourceObservation":null,"rendererAdoption":null,"errors":errors}))
}

pub(crate) async fn final_remote_backup_costly_slot(
    slot:&costly_runner::CostlySlot,store:&mut PersistentStore,state:&crate::persistent_store::commands::PersistentStoreState,
    connected:&crate::external_storage::connection_commands::ConnectedRepository,job:&crate::external_storage::job_store::DurableJob,
    provider:&Arc<crate::external_storage::fake::FakeProvider>,permit:crate::native_file_jobs::admission::Permit,
    asset_hashes:&[String],selected_character_id:Option<String>,
) ->Result<(Value,Option<crate::native_file_jobs::admission::Permit>),String> {
    if !matches!(slot.scenario,measurement::Scenario::RestoreAllPresent|measurement::Scenario::RestoreMissing)
        || slot.warmup!=(slot.iteration<costly_runner::WARMUPS) || slot.iteration>=costly_runner::WARMUPS+costly_runner::REPETITIONS {
        return Err("remote backup costly slot differs from the fixed matrix".into());
    }
    let (mut raw,guard)=native_maintenance::remote_backup_restore_raw(store,state,connected,job,provider,permit,asset_hashes,
        selected_character_id,slot.iteration,slot.scenario==measurement::Scenario::RestoreAllPresent).await?;
    raw["direction"]=serde_json::to_value(slot.direction).map_err(|e|e.to_string())?;Ok((raw,guard))
}

#[derive(Clone, Copy, Debug, Serialize)]
pub(crate) enum SmallChange { Setting, Preset, Append }

#[derive(Clone, Copy, Debug, Serialize)]
pub(crate) enum Direction { AtoB, BtoA }

#[derive(Debug, Serialize)]
struct ExternalKeyScope {
    attempted_keys: Vec<String>,
    held_keys: Vec<String>,
    deferred_keys: Vec<String>,
    segment_attempts: usize,
    accepted_publications: usize,
    receive_applies: usize,
    failed_operations: usize,
    pending_operations: usize,
}

#[derive(Debug, Serialize)]
pub(crate) struct NativeSample {
    transport: &'static str,
    direction: Direction,
    change: SmallChange,
    iteration: u64,
    fixture_scale: FixtureScale,
    generated_json_bytes: u64,
    source_database_bytes_before_change: u64,
    selected_keys: Option<Vec<String>>,
    emitted_keys: Option<Vec<String>>,
    accepted_keys: Option<Vec<String>>,
    affected_keys: Option<Vec<String>>,
    external_key_scope: Option<ExternalKeyScope>,
    observation: NativeObservation,
    durable_save_ms: f64,
    publication_complete_ms: f64,
    receiver_durable_complete_ms: f64,
}

pub(crate) fn frozen_small_scales() -> [FixtureScale; 2] {
    [FixtureScale::small(), FixtureScale { minimum_database_bytes: 16 * 1024 * 1024,
        ..FixtureScale::small() }]
}

fn seed(store: &mut PersistentStore, directory: &Path, scale: FixtureScale) -> FixtureReceipt {
    assert!(frozen_small_scales().contains(&scale), "small driver accepts only frozen A/B fixtures");
    let receipt = fixture::generate(directory, scale.clone(), false).unwrap();
    let mut database: Value = serde_json::from_reader(File::open(directory.join("database.json")).unwrap()).unwrap();
    let characters = database.as_object_mut().unwrap().remove("characters").unwrap();
    let presets = database.as_object_mut().unwrap().remove("botPresets").unwrap();
    database["botPresetsId"] = json!("synthetic-preset-0");
    database["loreBookDepth"] = json!(5);
    let staging = store.replace_begin().unwrap().staging_id;
    store.replace_put_root(&staging, &database).unwrap();
    store.replace_put_presets(&staging, presets.as_array().unwrap()).unwrap();
    for character in characters.as_array().unwrap() {
        store.replace_add_characters(&staging, std::slice::from_ref(character)).unwrap();
    }
    store.replace_commit(&staging, Some(store.revision().unwrap())).unwrap();
    for index in 0..scale.assets {
        let descriptor = fixture::asset_descriptor(scale.seed, index);
        let alias = server_sync::lww_tests::put_asset(store, &descriptor.logical_key,
            &fixture::asset_body(scale.seed, index));
        assert_eq!(alias.object_hash.as_deref(), Some(descriptor.payload_hash.as_str()));
    }
    assert_eq!(store.read_conversation("synthetic-character-0", "synthetic-conversation-0", None)
        .unwrap().unwrap().value["message"].as_array().unwrap().len(), 4096);
    receipt
}

fn bind(store: &mut PersistentStore, target: SyncTarget, target_id: &str, library: &str) {
    let state = store.lww_binding_state().unwrap();
    let inspection = store.register_lww_binding_inspection(state.target_authority, &target, target_id, library).unwrap();
    store.switch_lww_binding(&SwitchBindingRequest {
        initial_publication: false,
        header: server_sync::lww_tests::header(store), expected_selection_epoch: state.selection_epoch,
        target, inspection_id: Some(inspection),
    }).unwrap();
}

fn fixture_asset_inventory(store:&PersistentStore, scale:&FixtureScale) -> Vec<String> {
    let aliases = store.list_asset_aliases(None).unwrap().value;
    assert_eq!(aliases.len(), scale.assets as usize);
    let hashes = aliases.into_iter().map(|alias| {
        assert_eq!(alias.kind, "asset");
        alias.object_hash.expect("fixture asset must have a cataloged object")
    }).collect::<std::collections::BTreeSet<_>>();
    assert_eq!(hashes.len(), scale.assets as usize);
    let mut catalog = std::collections::BTreeSet::new();
    let mut cursor = None;
    loop {
        let page = store.query_asset_object_catalog(4096, cursor.as_deref()).unwrap();
        catalog.extend(page.items.into_iter().map(|item| item.object_hash));
        cursor = page.next_cursor;
        if cursor.is_none() { break; }
    }
    assert!(hashes.is_subset(&catalog), "fixture aliases must reference actual catalog rows");
    hashes.into_iter().collect()
}

fn reset_work(asset_hashes:&[String]) {
    super::hash_work::reset_receive_intent_inputs();
    super::hash_work::reset_commit_intent_inputs();
    crate::asset_repository::body_io::reset_body_io();
    for hash in asset_hashes {
        crate::asset_repository::body_io::register_object_purpose(hash,
            crate::asset_repository::body_io::BodyPurpose::Asset);
    }
    crate::external_storage::lww_engine::cycle_keys::reset();
    lww::reset_work_metrics();
    super::hash_work::reset_hash_work();
    super::message_pages::reset_capture_work();
    server_sync::hash_metrics::reset_hash_metrics();
    lww_segment::reset_hash_bytes();
    lww_segment::reset_delegated_hash_work();
}

fn take_work(external: bool) -> NativeObservation {
    let mut result = NativeObservation::default();
    // The second tuple field is an alias of native_unit_envelope, not another domain.
    result.units_visited = lww::take_work_metrics().0;
    let native = super::hash_work::take_hash_work();
    result.native_caller_hashes=Some(native.domains.iter().map(|(key,work)|(key.to_string(),native_observation::HashDomain {
        calls:work.calls,bytes:work.bytes})).collect());
    for (key, work) in native.domains { result.insert_domain("native", key, work.calls, work.bytes); }
    for (key, count) in native.incomplete {
        result.incomplete.push(format!("native/{key}: {count}"));
    }
    let c = server_sync::hash_metrics::take_hash_metrics();
    if !external {
        for (key, work) in c.domains { result.insert_domain("C", &key, work.calls, work.bytes); }
        if c.incomplete { result.incomplete.push("C/delegated".into()); }
    }
    let direct_calls = lww_segment::take_hash_calls();
    let direct_bytes = lww_segment::take_hash_bytes();
    let delegated = lww_segment::take_delegated_hash_work();
    if external {
        result.insert_domain("X1", "content-digest", direct_calls, direct_bytes);
        for (key, (calls, bytes)) in delegated { result.insert_domain("X1", key, calls, bytes); }
    }
    let capture = super::message_pages::take_capture_work();
    result.messages_read = capture.work.messages_read as u64;
    result.message_bytes_read = capture.work.bytes_read;
    result.pages_written = capture.work.pages_written as u64;
    result.pages_reused = (capture.work.prefix_reused + capture.work.suffix_reused) as u64;
    if capture.failed_captures > 0 { result.incomplete.push("native/failed-capture".into()); }
    let bodies = crate::asset_repository::body_io::take_body_io();
    result.body_scope_complete = Some(bodies.complete() && bodies.thread == std::thread::current().id());
    result.body_scope_thread = Some(format!("{:?}", bodies.thread));
    result.body_stat_requests = bodies.stat_requests;
    result.body_batch_stat_requests = bodies.batch_stat_requests;
    result.body_presence_queries = bodies.presence_queries;
    result.body_asset_work = body_domain(bodies.asset_work());
    result.body_control_work = body_domain(bodies.control_work());
    result.body_owned_work = body_domain(bodies.domains.get("owned").cloned().unwrap_or_default());
    result.body_unknown_work = body_domain(bodies.unknown_work());
    result.body_unattributed_managed = body_domain(bodies.unattributed_managed.clone());
    result.body_unattributed_owned = body_domain(bodies.unattributed_owned.clone());
    result.body_pending_owned_identities = bodies.pending_owned_identities;
    result.body_pending_hashes=bodies.pending_body_hashes;
    result.body_pending_worker_scopes=bodies.pending_worker_scopes;
    result.body_worker_scopes_started=bodies.worker_scopes_started;
    result.body_worker_scopes_settled=bodies.worker_scopes_settled;
    result.body_worker_threads=bodies.worker_threads.iter().cloned().collect();
    result.body_scope_violations = bodies.scope_violations;
    result.asset_body_opens = Some(result.body_asset_work.open_attempts);
    result.asset_body_bytes_read = Some(result.body_asset_work.read_bytes);
    for (hash, object) in bodies.objects {
        if object.work == crate::asset_repository::body_io::BodyWork::default()
            && object.owned_work == crate::asset_repository::body_io::BodyWork::default() {
            continue;
        }
        result.body_objects.insert(hash, ObjectBodyDomain {
            purposes:object.purposes.into_iter().map(|purpose| format!("{purpose:?}")).collect(),
            work:body_domain(object.work),
            owned_work:body_domain(object.owned_work),
        });
    }
    for (domain, work) in bodies.domains {
        result.body_domains.insert(domain.into(), body_domain(work));
    }
    let inputs=super::hash_work::take_receive_intent_inputs();
    let commit_inputs=super::hash_work::take_commit_intent_inputs();
    let observed=result.hashes.get("native/native_receive_intent").cloned().unwrap_or_default();
    result.receive_intents=Some(input_evidence::<lww::StageReceive>(inputs,observed,"receive-intent"));
    let observed=result.hashes.get("native/native_intent").cloned().unwrap_or_default();
    result.commit_intents=Some(input_evidence::<lww::CommitIntentInput>(commit_inputs,observed,"commit-intent"));
    result
}

fn input_evidence<T:serde::de::DeserializeOwned+Serialize>(inputs:Vec<Vec<u8>>,
    observed:native_observation::HashDomain,domain:&str) -> invalid_diagnostics::ReceiveIntentEvidence {
    let mut evidence=invalid_diagnostics::ReceiveIntentEvidence {
        observed_calls:observed.calls,observed_bytes:observed.bytes,captured_calls:inputs.len() as u64,
        ..Default::default()};
    for input in inputs {
        evidence.captured_bytes=evidence.captured_bytes.checked_add(input.len() as u64).unwrap();
        let structure=(|| -> Result<invalid_diagnostics::JsonByteStructure,String> {
            let request:T=serde_json::from_slice(&input).map_err(|e|e.to_string())?;
            let roundtrip=serde_json::to_vec(&request).map_err(|e|e.to_string())?;
            invalid_diagnostics::exact_json_structure(&input,&roundtrip)
        })();
        match structure {Ok(value)=>evidence.inputs.push(value),Err(error)=>evidence.errors.push(error)}
    }
    evidence.finish_for(domain);
    evidence
}

fn body_domain(work:crate::asset_repository::body_io::BodyWork) -> BodyDomain {
    BodyDomain {open_attempts:work.open_attempts, opens:work.opens, failed_opens:work.failed_opens,
        unknown_open_results:work.unknown_open_results, read_operations:work.read_operations,
        read_bytes:work.read_bytes, incomplete_reads:work.incomplete_reads,
        escaped_handles:work.escaped_handles, escaped_paths:work.escaped_paths,
        outstanding_readers:work.outstanding_readers,
        identity_metadata_open_attempts:work.identity_metadata_open_attempts,
        identity_metadata_opens:work.identity_metadata_opens,
        identity_metadata_failed_opens:work.identity_metadata_failed_opens,
        verified_identity_metadata_opens:work.verified_identity_metadata_opens,
        staging_write_attempts:work.staging_write_attempts,staging_writes:work.staging_writes,
        staging_failed_writes:work.staging_failed_writes,staging_requested_bytes:work.staging_requested_bytes,
        staging_written_bytes:work.staging_written_bytes,incomplete_writes:work.incomplete_writes,
        publication_attempts:work.publication_attempts,publications:work.publications,
        publication_already_exists:work.publication_already_exists,publication_failures:work.publication_failures,
        publication_object_bytes:work.publication_object_bytes,
        publication_kinds:work.publication_kinds.into_iter().map(|(kind,value)|(kind.into(),native_observation::PublicationDomain {
            attempts:value.attempts,successes:value.successes,already_exists:value.already_exists,failures:value.failures,object_bytes:value.object_bytes})).collect(),
        body_sha:work.body_sha.into_iter().map(|(name,value)|(name.into(),native_observation::HashDomain {calls:value.calls,bytes:value.bytes})).collect()}
}

fn require_routine_observation(observation:&NativeObservation, transport:&str,
    direction:Direction, change:SmallChange, iteration:u64) {
    if let Err(error) = observation.validate_routine_invariants() {
        eprintln!("{}", serde_json::to_string_pretty(&json!({
            "kind":"synthetic-native-observation-failure", "transport":transport,
            "direction":direction, "change":change, "iteration":iteration,
            "validation_thread":format!("{:?}",std::thread::current().id()),
            "error":error, "observation":observation,
        })).unwrap());
        panic!("invalid native sample: {error}");
    }
}

fn prepare_edit(store: &PersistentStore, change: SmallChange, direction: Direction, iteration: u64) -> (WorkingSetCommit, Value) {
    let root = store.read_root(None).unwrap().value;
    let mut commit = WorkingSetCommit { expected_revision:store.revision().unwrap(),
        binding_authority:Some(store.lww_binding_authority().unwrap()),
        request_id:Some(uuid::Uuid::new_v4().to_string()), ..Default::default() };
    let expected = match change {
        SmallChange::Setting => {
            let next = json!(root["loreBookDepth"].as_i64().unwrap() + 1);
            commit.unit_mutations = Some(vec![UnitMutation::Set {
                key:UnitKey::new(&["root", "loreBookDepth"]).unwrap(), value:next.clone() }]);
            next
        },
        SmallChange::Preset => {
            let next = json!(if root["botPresetsId"] == "synthetic-preset-0" {
                "synthetic-preset-1" } else { "synthetic-preset-0" });
            commit.unit_mutations = Some(vec![UnitMutation::Set {
                key:UnitKey::new(&["root", "botPresetsId"]).unwrap(), value:next.clone() }]);
            next
        },
        SmallChange::Append => {
            let conversation = store.read_conversation("synthetic-character-0", "synthetic-conversation-0", None)
                .unwrap().unwrap().value;
            let start = conversation["message"].as_array().unwrap().len() as i64;
            let message = json!({"role":"user", "data":"Fixed synthetic appended message",
                "chatId":format!("synthetic-append-{direction:?}-{iteration}")});
            commit.conversations = Some(vec![ConversationMutation::ReplaceRange {
                character_id:"synthetic-character-0".into(), conversation_id:"synthetic-conversation-0".into(),
                start, delete_count:0, messages:vec![message.clone()], conversation:None, configured_index:None }]);
            commit.messages_changed = Some(vec![MessageLocator {
                character_id:"synthetic-character-0".into(), conversation_id:"synthetic-conversation-0".into(), start:Some(start) }]);
            let mut expected_messages = conversation["message"].as_array().unwrap().clone();
            expected_messages.push(message);
            Value::Array(expected_messages)
        },
    };
    (commit, expected)
}

fn verify(store: &PersistentStore, change: SmallChange, expected: &Value) {
    match change {
        SmallChange::Setting => assert_eq!(&store.read_root(None).unwrap().value["loreBookDepth"], expected),
        SmallChange::Preset => assert_eq!(&store.read_root(None).unwrap().value["botPresetsId"], expected),
        SmallChange::Append => {
            let conversation = store.read_conversation("synthetic-character-0", "synthetic-conversation-0", None)
                .unwrap().unwrap().value;
            let messages = conversation["message"].as_array().unwrap();
            assert!(messages == expected.as_array().unwrap(), "append changed prior message content or fields");
            let unique = messages.iter().map(|message| message["chatId"].as_str().unwrap())
                .collect::<std::collections::BTreeSet<_>>();
            assert_eq!(unique.len(), messages.len());
        },
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum AdditionalChange { Persona, MiddleEdit, Insertion, Deletion, Oversized, Burst, Sparse }

fn sparse_message(mut message:Value,marker:&str)->Result<Value,String> {
    for nonce in 0..1024u32 {
        message["data"]=json!(format!("Synthetic sparse message {marker}-{nonce}"));
        let bytes=risunest_sync_wire::payload_value::encode(&message).map_err(|e|e.to_string())?;
        let hash=risunest_external_storage_format::message_pages::MessageHash::from_bytes(&bytes);
        if !hash.boundary().map_err(|e|e.to_string())? {return Ok(message);}
    }
    Err("bounded sparse message search exhausted".into())
}

/// Fixture preparation is committed and transported before routine counters begin.
fn prepare_sparse_fixture(store:&PersistentStore)->Result<PreparedAdditionalChange,String> {
    let conversation=store.read_conversation("synthetic-character-0","synthetic-conversation-0",None)
        .map_err(|e|e.to_string())?.ok_or("sparse fixture conversation absent")?.value;
    let prior=conversation["message"].as_array().ok_or("sparse fixture messages absent")?;
    if !(4096..=8192).contains(&prior.len()) {return Err("sparse fixed conversation is outside declared bounds".into());}
    let messages=prior.iter().enumerate().map(|(index,message)|sparse_message(message.clone(),&index.to_string()))
        .collect::<Result<Vec<_>,_>>()?;
    let commit=WorkingSetCommit {expected_revision:store.revision().map_err(|e|e.to_string())?,
        binding_authority:Some(store.lww_binding_authority().map_err(|e|e.to_string())?),
        request_id:Some(uuid::Uuid::new_v4().to_string()),
        conversations:Some(vec![ConversationMutation::ReplaceRange {
            character_id:"synthetic-character-0".into(),conversation_id:"synthetic-conversation-0".into(),
            start:0,delete_count:prior.len() as i64,messages:messages.clone(),conversation:None,configured_index:None}]),
        messages_changed:Some(vec![MessageLocator {character_id:"synthetic-character-0".into(),
            conversation_id:"synthetic-conversation-0".into(),start:Some(0)}]),..Default::default()};
    Ok(PreparedAdditionalChange {commits:vec![commit],expected_root:Default::default(),expected_messages:Some(messages)})
}

pub(crate) struct PreparedAdditionalChange {
    pub commits:Vec<WorkingSetCommit>,
    expected_root:std::collections::BTreeMap<String,Value>,
    expected_messages:Option<Vec<Value>>,
}

pub(crate) fn prepare_additional_change(store:&PersistentStore, change:AdditionalChange,
    direction:Direction, iteration:u64) -> PreparedAdditionalChange {
    let root = store.read_root(None).unwrap().value;
    let revision = store.revision().unwrap();
    let authority = store.lww_binding_authority().unwrap();
    let mut prepared = PreparedAdditionalChange {commits:Vec::new(),
        expected_root:std::collections::BTreeMap::new(),expected_messages:None};
    let make_commit = |offset:i64| WorkingSetCommit {
        expected_revision:revision.checked_add(offset).unwrap(),binding_authority:Some(authority),
        request_id:Some(uuid::Uuid::new_v4().to_string()),..Default::default()};
    match change {
        AdditionalChange::Persona => {
            let current = root["selectedPersona"].as_str().expect("final fixture needs stable persona selection");
            assert!(matches!(current,"synthetic-persona-0" | "synthetic-persona-1"));
            let next = json!(if current=="synthetic-persona-0" {"synthetic-persona-1"} else {"synthetic-persona-0"});
            let mut commit = make_commit(0);
            commit.unit_mutations = Some(vec![UnitMutation::Set {
                key:UnitKey::new(&["root","selectedPersona"]).unwrap(),value:next.clone()}]);
            prepared.expected_root.insert("selectedPersona".into(),next);
            prepared.commits.push(commit);
        },
        AdditionalChange::Burst => {
            let initial = root["loreBookDepth"].as_i64().unwrap();
            // Sixteen durable changes to one shared unit test actual outbox coalescing.
            for offset in 0..16 {
                let mut commit = make_commit(offset);
                let value = json!(initial.checked_add(offset+1).unwrap());
                commit.unit_mutations = Some(vec![UnitMutation::Set {
                    key:UnitKey::new(&["root","loreBookDepth"]).unwrap(),value}]);
                prepared.commits.push(commit);
            }
            prepared.expected_root.insert("loreBookDepth".into(),json!(initial.checked_add(16).unwrap()));
        },
        AdditionalChange::MiddleEdit | AdditionalChange::Insertion | AdditionalChange::Deletion | AdditionalChange::Oversized | AdditionalChange::Sparse => {
            let conversation = store.read_conversation("synthetic-character-0","synthetic-conversation-0",None)
                .unwrap().unwrap().value;
            let mut expected = conversation["message"].as_array().unwrap().clone();
            let index = 2048;
            assert!(expected.len()>index);
            let (delete_count,messages) = match change {
                AdditionalChange::MiddleEdit | AdditionalChange::Oversized => {
                    let mut message = expected[index].clone();
                    message["data"] = json!(if matches!(change,AdditionalChange::Oversized) {
                        format!("{}{direction:?}-{iteration}","x".repeat(128*1024))
                    } else {format!("Fixed synthetic middle edit {direction:?}-{iteration}")});
                    expected[index]=message.clone();
                    (1,vec![message])
                },
                AdditionalChange::Insertion | AdditionalChange::Sparse => {
                    let mut message = json!({"role":"user","data":"Fixed synthetic inserted message",
                        "chatId":format!("synthetic-insert-{direction:?}-{iteration}")});
                    if matches!(change,AdditionalChange::Sparse) {
                        message["chatId"]=json!(format!("synthetic-sparse-insert-{direction:?}-{iteration}"));
                        for existing in &expected {
                            let bytes=risunest_sync_wire::payload_value::encode(existing).unwrap();
                            assert!(!risunest_external_storage_format::message_pages::MessageHash::from_bytes(&bytes).boundary().unwrap());
                        }
                        message=sparse_message(message,&format!("insert-{direction:?}-{iteration}")).unwrap();
                    }
                    expected.insert(index,message.clone());
                    (0,vec![message])
                },
                AdditionalChange::Deletion => {expected.remove(index); (1,Vec::new())},
                _ => unreachable!(),
            };
            let mut commit = make_commit(0);
            commit.conversations=Some(vec![ConversationMutation::ReplaceRange {
                character_id:"synthetic-character-0".into(),conversation_id:"synthetic-conversation-0".into(),
                start:index as i64,delete_count,messages,conversation:None,configured_index:None}]);
            commit.messages_changed=Some(vec![MessageLocator {character_id:"synthetic-character-0".into(),
                conversation_id:"synthetic-conversation-0".into(),start:Some(index as i64)}]);
            prepared.expected_messages=Some(expected);
            prepared.commits.push(commit);
        },
    }
    prepared
}

pub(crate) fn verify_additional_change(store:&PersistentStore, prepared:&PreparedAdditionalChange) {
    let root = store.read_root(None).unwrap().value;
    for (key,value) in &prepared.expected_root {assert_eq!(&root[key.as_str()],value);}
    if let Some(messages) = &prepared.expected_messages {
        verify(store,SmallChange::Append,&Value::Array(messages.clone()));
    }
}

pub(crate) fn apply_additional_change(store:&mut PersistentStore,prepared:&PreparedAdditionalChange) {
    for commit in &prepared.commits {store.commit(commit).unwrap();}
}

fn prepare_final_change(store:&PersistentStore,scenario:measurement::Scenario,
    direction:Direction,iteration:u64)->Result<PreparedAdditionalChange,String> {
    use measurement::Scenario;
    let small=match scenario {Scenario::SettingEdit=>Some(SmallChange::Setting),
        Scenario::PresetSwitch=>Some(SmallChange::Preset),Scenario::Append=>Some(SmallChange::Append),_=>None};
    if let Some(change)=small {
        let (commit,expected)=prepare_edit(store,change,direction,iteration);
        let mut prepared=PreparedAdditionalChange {commits:vec![commit],expected_root:Default::default(),expected_messages:None};
        match change {SmallChange::Setting=>{prepared.expected_root.insert("loreBookDepth".into(),expected);},
            SmallChange::Preset=>{prepared.expected_root.insert("botPresetsId".into(),expected);},
            SmallChange::Append=>prepared.expected_messages=Some(expected.as_array().unwrap().clone())}
        return Ok(prepared);
    }
    let additional=match scenario {Scenario::PersonaSwitch=>AdditionalChange::Persona,Scenario::MiddleEdit=>AdditionalChange::MiddleEdit,
        Scenario::Insertion=>AdditionalChange::Insertion,Scenario::Deletion=>AdditionalChange::Deletion,
        Scenario::OversizedMessage=>AdditionalChange::Oversized,Scenario::Burst=>AdditionalChange::Burst,
        Scenario::SparseBoundary=>AdditionalChange::Sparse,
        _=>return Err("scenario requires an unverified production endpoint".into())};
    Ok(prepare_additional_change(store,additional,direction,iteration))
}

fn final_direction(direction:final_runner::Direction)->Direction {
    match direction {final_runner::Direction::AtoB=>Direction::AtoB,final_runner::Direction::BtoA=>Direction::BtoA}
}

pub(crate) struct FinalExternalAdapter<'a> {
    pub fixture:&'a mut CycleFixture,
    pub asset_hashes:&'a [String],
}
impl final_runner::AsyncCycleDriver for FinalExternalAdapter<'_> {
    async fn cycle(&mut self,scenario:measurement::Scenario,direction:final_runner::Direction,iteration:u32,warmup:bool)
        ->Result<final_runner::RoutineSample,final_runner::CycleFailure> {
        let fixture=&mut self.fixture;
        let (source,destination,sender,receiver)=match direction {
            final_runner::Direction::AtoB=>(&mut fixture.a,&mut fixture.b,&mut fixture.sender,&mut fixture.receiver),
            final_runner::Direction::BtoA=>(&mut fixture.b,&mut fixture.a,&mut fixture.receiver,&mut fixture.sender)};
        final_external_cycle(source,destination,sender,receiver,&fixture.provider,self.asset_hashes,
            scenario,direction,iteration,warmup).await
    }
}

/// Both independently registered endpoints must already be bound and drained.
pub(crate) fn collect_final_local(a:&mut PersistentStore,b:&mut PersistentStore,ca:&LwwClient,cb:&LwwClient,asset_hashes:&[String])
    ->final_runner::CollectedSamples {
    final_runner::collect(|scenario,direction,iteration,warmup| {
        let (source,destination,sender,receiver)=match direction {
            final_runner::Direction::AtoB=>(&mut *a,&mut *b,ca,cb),
            final_runner::Direction::BtoA=>(&mut *b,&mut *a,cb,ca)};
        final_server_cycle(source,destination,sender,receiver,asset_hashes,scenario,direction,iteration,warmup)
    })
}

/// Counters end before journal diagnostics and full message verification.
fn verify_post_scope<T>(observation:&NativeObservation,verify:impl FnOnce()->Result<T,String>)
    ->Result<T,final_runner::CycleFailure> {
    let result=std::panic::catch_unwind(std::panic::AssertUnwindSafe(verify));
    let reason=match result {
        Ok(Ok(value))=>return Ok(value),
        Ok(Err(reason))=>reason,
        Err(_)=>"post-scope correctness assertion failed".into(),
    };
    Err(final_runner::CycleFailure {reason,observation:Some(Box::new(observation.clone()))})
}

#[test]
#[ignore = "measurement harness self-test"]
fn final_post_scope_errors_and_assertions_preserve_actual_observation() {
    let observation=NativeObservation {units_visited:37,requests:11,..Default::default()};
    let sql_error=verify_post_scope::<()>(&observation,||Err("journal diagnostic failed".into())).unwrap_err();
    assert_eq!(sql_error.reason,"journal diagnostic failed");
    assert_eq!(sql_error.observation.unwrap().units_visited,37);
    let assertion=verify_post_scope::<()>(&observation,|| {assert_eq!(1,2,"full message mismatch");Ok(())}).unwrap_err();
    assert_eq!(assertion.reason,"post-scope correctness assertion failed");
    assert_eq!(assertion.observation.unwrap().requests,11);
    assert_eq!(verify_post_scope(&observation,||Ok(19)).unwrap(),19);
}

fn final_server_cycle(source:&mut PersistentStore,destination:&mut PersistentStore,sender:&LwwClient,receiver:&LwwClient,
    asset_hashes:&[String],scenario:measurement::Scenario,direction:final_runner::Direction,iteration:u32,warmup:bool)
    ->Result<final_runner::RoutineSample,final_runner::CycleFailure> {
    if scenario==measurement::Scenario::SparseBoundary && iteration==0 {
        let setup=prepare_sparse_fixture(source)?;
        for commit in &setup.commits {source.commit(commit).map_err(|e|e.to_string())?;}
        server_sync::lww_tests::drain_publications(sender,source,&[]).map_err(|e|format!("{e:?}"))?;
        server_sync::lww_tests::receive_available(receiver,destination,&[]).map_err(|e|format!("{e:?}"))?;
        verify_additional_change(destination,&setup);
    }
    let prepared=prepare_final_change(source,scenario,final_direction(direction),iteration.into())?;
    let io=[sender.client.test_io.as_ref().ok_or("sender HTTP observer absent")?,
        receiver.client.test_io.as_ref().ok_or("receiver HTTP observer absent")?];
    if Arc::ptr_eq(io[0],io[1]) {return Err("endpoint HTTP observers overlap".into());}
    for counter in io {counter.reset();}
    reset_work(asset_hashes);
    let started=Instant::now();
    let result=(|| {
        for commit in &prepared.commits {source.commit(commit).map_err(|e|e.to_string())?;}
        let durable=started.elapsed().as_secs_f64()*1000.0;
        let receipt=server_sync::lww_tests::publish_cycle(sender,source,&[]).map_err(|e|format!("{e:?}"))?
            .ok_or("fixed final change emitted no operation")?;
        let published=started.elapsed().as_secs_f64()*1000.0;
        let completion=server_sync::lww_tests::receive_cycle(receiver,destination,&[]).map_err(|e|format!("{e:?}"))?;
        let finished=started.elapsed().as_secs_f64()*1000.0;
        Ok::<_,String>((durable,published,finished,receipt,completion))
    })();
    let mut observation=take_work(false);
    // The actual local-server LWW path has no shared mutable head API.
    observation.shared_head_reads=Some(0);
    for counter in io {let [requests,sent,received]=counter.snapshot();
        observation.requests+=requests;observation.uploaded_bytes+=sent;observation.downloaded_bytes+=received;}
    let (durable_save_ms,publication_complete_ms,receiver_durable_ack_ms,receipt,completion)=result
        .map_err(|reason|final_runner::CycleFailure {reason,observation:Some(Box::new(observation.clone()))})?;
    verify_post_scope(&observation.clone(),|| {
    if completion.received_units==0 {return Err("fixed final change had no receive units".into());}
    let (body,intent):(Vec<u8>,String)=sender.log.0.query_row("SELECT body,intent FROM publications WHERE id=?1",
        [&receipt.operation_id],|row|Ok((row.get(0)?,row.get(1)?))).map_err(|e|e.to_string())?;
    let request:risunest_sync_wire::lww::PushRequest=serde_json::from_slice(&body).map_err(|e|e.to_string())?;
    let publication:server_sync::lww_client::Publication=serde_json::from_str(&intent).map_err(|e|e.to_string())?;
    if publication.request!=request || request.operation_id!=receipt.operation_id {return Err("immutable operation evidence mismatch".into());}
    verify_additional_change(destination,&prepared);
    if !source.lww_read_outbox(source.lww_binding_authority().map_err(|e|e.to_string())?,1)
        .map_err(|e|e.to_string())?.entries.is_empty() {return Err("routine publication left an outbox entry".into());}
    Ok(final_runner::RoutineSample {scenario,direction,iteration,warmup,
        selected_keys:key_set(publication.entries.into_iter().map(|entry|entry.key)),
        emitted_keys:key_set(request.changes.into_iter().map(|change|change.key)),
        accepted_winner_keys:Some(key_set(receipt.accepted_keys)),affected_keys:key_set(completion.result.affected_keys),
        dispatch_keys:None,failed_operations:0,pending_operations:0,observation,
        timings:final_runner::NativeTimings {durable_save_ms,publication_complete_ms,receiver_durable_ack_ms}})
    })
}

async fn final_external_cycle(source:&mut PersistentStore,destination:&mut PersistentStore,
    sender:&mut crate::external_storage::lww_engine::ExternalLwwEngine,
    receiver:&mut crate::external_storage::lww_engine::ExternalLwwEngine,
    provider:&crate::external_storage::fake::FakeProvider,asset_hashes:&[String],scenario:measurement::Scenario,
    direction:final_runner::Direction,iteration:u32,warmup:bool)->Result<final_runner::RoutineSample,final_runner::CycleFailure> {
    if scenario==measurement::Scenario::SparseBoundary && iteration==0 {
        let setup=prepare_sparse_fixture(source)?;
        for commit in &setup.commits {source.commit(commit).map_err(|e|e.to_string())?;}
        sender.publish(source,source.lww_binding_authority().map_err(|e|e.to_string())?,&[],&Cancellation::default())
            .await.map_err(|e|e.to_string())?;
        receiver.receive_and_apply(destination,destination.lww_binding_authority().map_err(|e|e.to_string())?,&[],&Cancellation::default())
            .await.map_err(|e|e.to_string())?;
        verify_additional_change(destination,&setup);verify_settled(source,sender);verify_settled(destination,receiver);
    }
    let prepared=prepare_final_change(source,scenario,final_direction(direction),iteration.into())?;
    let requests_before=provider.read_count()+provider.upload_count()+provider.listing_count();
    let bytes_before=provider.transferred_body_bytes();let heads_before=provider.read_attempts("head");
    reset_work(asset_hashes);
    let started=Instant::now();
    let result=async {
        for commit in &prepared.commits {source.commit(commit).map_err(|e|e.to_string())?;}
        let durable=started.elapsed().as_secs_f64()*1000.0;
        sender.publish(source,source.lww_binding_authority().map_err(|e|e.to_string())?,&[],&Cancellation::default())
            .await.map_err(|e|e.to_string())?;
        let published=started.elapsed().as_secs_f64()*1000.0;
        receiver.receive_and_apply(destination,destination.lww_binding_authority().map_err(|e|e.to_string())?,&[],&Cancellation::default())
            .await.map_err(|e|e.to_string())?;
        Ok::<_,String>((durable,published,started.elapsed().as_secs_f64()*1000.0))
    }.await;
    let mut observation=take_work(true);
    let keys=crate::external_storage::lww_engine::cycle_keys::take();
    let bytes_after=provider.transferred_body_bytes();
    observation.requests=(provider.read_count()+provider.upload_count()+provider.listing_count()-requests_before) as u64;
    observation.uploaded_bytes=bytes_after.0-bytes_before.0;observation.downloaded_bytes=bytes_after.1-bytes_before.1;
    observation.shared_head_reads=Some((provider.read_attempts("head")-heads_before) as u64);
    let (durable_save_ms,publication_complete_ms,receiver_durable_ack_ms)=result
        .map_err(|reason|final_runner::CycleFailure {reason,observation:Some(Box::new(observation.clone()))})?;
    verify_post_scope(&observation.clone(),|| {
    verify_additional_change(destination,&prepared);
    verify_settled(source,sender);verify_settled(destination,receiver);
    Ok(final_runner::RoutineSample {scenario,direction,iteration,warmup,
        selected_keys:key_set(keys.selected_keys),emitted_keys:key_set(keys.emitted_keys),accepted_winner_keys:None,
        affected_keys:key_set(keys.affected_keys),dispatch_keys:Some(key_set(keys.attempted_keys)),
        failed_operations:keys.failed_operations as u64,pending_operations:keys.pending_operations as u64,observation,
        timings:final_runner::NativeTimings {durable_save_ms,publication_complete_ms,receiver_durable_ack_ms}})
    })
}

fn verify_bootstrap(a:&PersistentStore, b:&PersistentStore) {
    let mut local = a.read_root(None).unwrap().value;
    let mut remote = b.read_root(None).unwrap().value;
    // temperature is a derived preset field, excluded from shared root units.
    local.as_object_mut().unwrap().remove("temperature");
    remote.as_object_mut().unwrap().remove("temperature");
    assert_eq!(local, remote);
    assert_eq!(a.read_conversation("synthetic-character-0", "synthetic-conversation-0", None)
        .unwrap().unwrap().value, b.read_conversation("synthetic-character-0",
            "synthetic-conversation-0", None).unwrap().unwrap().value);
}

fn verify_settled(store:&PersistentStore, engine:&crate::external_storage::lww_engine::ExternalLwwEngine) {
    assert!(store.lww_read_outbox(store.lww_binding_authority().unwrap(), 1).unwrap().entries.is_empty());
    assert!(store.external_lww_pending(&engine.target_scope(), &store.lww_clock_state().unwrap().writer_id)
        .unwrap().is_none());
    let device = store.device_store().unwrap().connection();
    for query in ["SELECT COUNT(*) FROM lww_intents WHERE complete=0",
        "SELECT COUNT(*) FROM lww_receive WHERE finished=0",
        "SELECT COUNT(*) FROM lww_receive_rows WHERE status IN ('staged','held','deferred')"] {
        assert_eq!(device.query_row(query, [], |row| row.get::<_,i64>(0)).unwrap(), 0);
    }
}

pub(crate) struct ExternalDriver {
    pub fixture: CycleFixture,
    _generated: tempfile::TempDir,
    pub receipt: FixtureReceipt,
    asset_hashes: Vec<String>,
}

impl ExternalDriver {
    pub(crate) async fn prepare(scale: FixtureScale) -> Self {
        let mut fixture = CycleFixture::new();
        assert_ne!(fixture.a.lww_clock_state().unwrap().writer_id, fixture.b.lww_clock_state().unwrap().writer_id);
        bind(&mut fixture.a, SyncTarget::External(fixture.sender.connection_id.clone()),
            &fixture.sender.repository.connection_identity, &fixture.sender.library);
        bind(&mut fixture.b, SyncTarget::External(fixture.receiver.connection_id.clone()),
            &fixture.receiver.repository.connection_identity, &fixture.receiver.library);
        let generated = tempfile::tempdir().unwrap();
        let receipt = seed(&mut fixture.a, &generated.path().join("fixture"), scale);
        let asset_hashes = fixture_asset_inventory(&fixture.a, &receipt.scale);
        let authority = fixture.a.lww_binding_authority().unwrap();
        fixture.sender.publish(&mut fixture.a, authority, &[], &Cancellation::default()).await.unwrap();
        let authority = fixture.b.lww_binding_authority().unwrap();
        fixture.receiver.receive_and_apply(&mut fixture.b, authority, &[], &Cancellation::default()).await.unwrap();
        verify_settled(&fixture.a, &fixture.sender);
        verify_settled(&fixture.b, &fixture.receiver);
        let writer = fixture.a.lww_clock_state().unwrap().writer_id;
        let published_prefix = fixture.a.external_lww_next_sequence(&fixture.sender.target_scope(), &writer).unwrap() - 1;
        // Publication ACK advances the sender's own receive prefix durably.
        for store in [&fixture.a, &fixture.b] {
            let progress = store.lww_receive_progress(store.lww_binding_authority().unwrap()).unwrap();
            let own = progress.iter().find(|item| item.kind == "external" && item.writer_id.as_deref() == Some(writer.as_str())).unwrap();
            assert_eq!(own.cursor.0, published_prefix);
        }
        verify_bootstrap(&fixture.a, &fixture.b);
        Self {fixture, _generated:generated, receipt, asset_hashes}
    }

    pub(crate) async fn run(&mut self, direction:Direction, change:SmallChange, iteration:u64) -> NativeSample {
        let provider = self.fixture.provider.clone();
        let requests_before = (provider.read_count() + provider.upload_count() + provider.listing_count()) as u64;
        let bytes_before = provider.transferred_body_bytes();
        let head_reads_before = provider.read_attempts("head");
        let (source, destination, sender, receiver) = match direction {
            Direction::AtoB => (&mut self.fixture.a, &mut self.fixture.b, &mut self.fixture.sender, &mut self.fixture.receiver),
            Direction::BtoA => (&mut self.fixture.b, &mut self.fixture.a, &mut self.fixture.receiver, &mut self.fixture.sender),
        };
        let (commit, expected) = prepare_edit(source, change, direction, iteration);
        let source_database_bytes_before_change = source.storage_stats().unwrap().database_bytes;
        reset_work(&self.asset_hashes);
        let started = Instant::now();
        source.commit(&commit).unwrap();
        let durable_save_ms = started.elapsed().as_secs_f64() * 1000.0;
        sender.publish(source, source.lww_binding_authority().unwrap(), &[], &Cancellation::default()).await.unwrap();
        let publication_complete_ms = started.elapsed().as_secs_f64() * 1000.0;
        receiver.receive_and_apply(destination, destination.lww_binding_authority().unwrap(), &[], &Cancellation::default()).await.unwrap();
        let receiver_durable_complete_ms = started.elapsed().as_secs_f64() * 1000.0;
        let mut observation = take_work(true);
        let keys = crate::external_storage::lww_engine::cycle_keys::take();
        assert!(keys.complete(), "failed or unfinished external key observation");
        let selected_keys = key_set(keys.selected_keys);
        let emitted_keys = key_set(keys.emitted_keys);
        let affected_keys = key_set(keys.affected_keys);
        let external_key_scope = ExternalKeyScope {
            attempted_keys:key_set(keys.attempted_keys), held_keys:key_set(keys.held_keys),
            deferred_keys:key_set(keys.deferred_keys), segment_attempts:keys.segment_attempts,
            accepted_publications:keys.accepted_publications, receive_applies:keys.receive_applies,
            failed_operations:keys.failed_operations, pending_operations:keys.pending_operations,
        };
        observation.requests = (provider.read_count() + provider.upload_count() + provider.listing_count()) as u64 - requests_before;
        let bytes_after = provider.transferred_body_bytes();
        observation.uploaded_bytes = bytes_after.0 - bytes_before.0;
        observation.downloaded_bytes = bytes_after.1 - bytes_before.1;
        observation.shared_head_reads = Some((provider.read_attempts("head") - head_reads_before) as u64);
        require_routine_observation(&observation, "fake-provider", direction, change, iteration);
        verify(destination, change, &expected);
        assert!(source.lww_read_outbox(source.lww_binding_authority().unwrap(), 1).unwrap().entries.is_empty());
        NativeSample {transport:"native-fake-provider", direction, change, iteration, observation,
            fixture_scale:self.receipt.scale.clone(), generated_json_bytes:self.receipt.database_bytes,
            source_database_bytes_before_change,
            selected_keys:Some(selected_keys), emitted_keys:Some(emitted_keys), accepted_keys:None,
            affected_keys:Some(affected_keys), external_key_scope:Some(external_key_scope),
            durable_save_ms, publication_complete_ms, receiver_durable_complete_ms}
    }
}

pub(crate) struct ServerDriver {
    _server: LocalServerFixture,
    _directories: [tempfile::TempDir; 3],
    a: PersistentStore,
    b: PersistentStore,
    ca: LwwClient,
    cb: LwwClient,
    pub receipt: FixtureReceipt,
    asset_hashes: Vec<String>,
}

impl ServerDriver {
    pub(crate) fn prepare(scale:FixtureScale) -> Self {
        let server = LocalServerFixture::new();
        let (directory_a, mut a) = server_sync::lww_tests::local();
        let (directory_b, mut b) = server_sync::lww_tests::local();
        assert_ne!(a.lww_clock_state().unwrap().writer_id, b.lww_clock_state().unwrap().writer_id);
        let ca = server.client(&a);
        let cb = server.client(&b);
        let config = ca.client.config();
        assert_eq!(config.library_id, cb.client.config().library_id);
        assert_ne!(config.device_id, cb.client.config().device_id);
        for store in [&mut a, &mut b] {
            bind(store, SyncTarget::Server(server.endpoint.clone()), &server.endpoint, &config.library_id);
        }
        let generated = tempfile::tempdir().unwrap();
        let receipt = seed(&mut a, &generated.path().join("fixture"), scale);
        let asset_hashes = fixture_asset_inventory(&a, &receipt.scale);
        server_sync::lww_tests::drain_publications(&ca, &mut a, &[]).unwrap();
        server_sync::lww_tests::receive_available(&cb, &mut b, &[]).unwrap();
        server_sync::lww_tests::receive_available(&ca, &mut a, &[]).unwrap();
        assert!(a.lww_read_outbox(a.lww_binding_authority().unwrap(), 1).unwrap().entries.is_empty());
        verify_bootstrap(&a, &b);
        Self {_server:server, _directories:[directory_a,directory_b,generated], a,b,ca,cb,receipt,asset_hashes}
    }

    pub(crate) fn run(&mut self, direction:Direction, change:SmallChange, iteration:u64) -> NativeSample {
        let (source, destination, sender, receiver) = match direction {
            Direction::AtoB => (&mut self.a,&mut self.b,&self.ca,&self.cb),
            Direction::BtoA => (&mut self.b,&mut self.a,&self.cb,&self.ca),
        };
        let io = [sender.client.test_io.as_ref().unwrap(), receiver.client.test_io.as_ref().unwrap()];
        assert!(!Arc::ptr_eq(io[0], io[1]));
        for counter in io { counter.reset(); }
        let (commit, expected) = prepare_edit(source, change, direction, iteration);
        let source_database_bytes_before_change = source.storage_stats().unwrap().database_bytes;
        reset_work(&self.asset_hashes);
        let started = Instant::now();
        source.commit(&commit).unwrap();
        let durable_save_ms = started.elapsed().as_secs_f64() * 1000.0;
        let receipt = server_sync::lww_tests::publish_cycle(sender, source, &[]).unwrap().unwrap();
        let publication_complete_ms = started.elapsed().as_secs_f64() * 1000.0;
        let completion = server_sync::lww_tests::receive_cycle(receiver, destination, &[]).unwrap();
        assert!(completion.received_units > 0);
        let receiver_durable_complete_ms = started.elapsed().as_secs_f64() * 1000.0;
        let mut observation = take_work(false);
        // The native local-server LWW protocol has no shared mutable head API.
        observation.shared_head_reads = Some(0);
        for counter in io {
            let [requests,sent,received] = counter.snapshot();
            observation.requests += requests;
            observation.uploaded_bytes += sent;
            observation.downloaded_bytes += received;
        }
        require_routine_observation(&observation, "local-server", direction, change, iteration);
        // Read the actual submitted operation only after timing and collector take.
        let (body, intent):(Vec<u8>, String) = sender.log.0.query_row(
            "SELECT body,intent FROM publications WHERE id=?1", [&receipt.operation_id],
            |row| Ok((row.get(0)?,row.get(1)?))).unwrap();
        let request: risunest_sync_wire::lww::PushRequest = serde_json::from_slice(&body).unwrap();
        let publication: server_sync::lww_client::Publication = serde_json::from_str(&intent).unwrap();
        assert_eq!(request.operation_id, receipt.operation_id);
        assert_eq!(publication.request, request);
        let selected_keys = key_set(publication.entries.into_iter().map(|entry| entry.key));
        // Submitted immutable body keys include dominated changes; accepted keys are winners.
        let emitted_keys = key_set(request.changes.into_iter().map(|change| change.key));
        assert_eq!(selected_keys, emitted_keys);
        let accepted_keys = key_set(receipt.accepted_keys);
        let affected_keys = key_set(completion.result.affected_keys);
        verify(destination, change, &expected);
        assert!(source.lww_read_outbox(source.lww_binding_authority().unwrap(), 1).unwrap().entries.is_empty());
        NativeSample {transport:"native-local-http", direction, change, iteration, observation,
            fixture_scale:self.receipt.scale.clone(), generated_json_bytes:self.receipt.database_bytes,
            source_database_bytes_before_change,
            selected_keys:Some(selected_keys), emitted_keys:Some(emitted_keys),
            accepted_keys:Some(accepted_keys), affected_keys:Some(affected_keys),
            external_key_scope:None,
            durable_save_ms, publication_complete_ms, receiver_durable_complete_ms}
    }
}

fn key_set(keys:impl IntoIterator<Item=UnitKey>) -> Vec<String> {
    keys.into_iter().map(|key| key.as_str().to_owned()).collect::<std::collections::BTreeSet<_>>()
        .into_iter().collect()
}

fn assert_same_key_sets(a:&NativeSample, b:&NativeSample) {
    for (name, small, larger) in [("selected", &a.selected_keys, &b.selected_keys),
        ("emitted", &a.emitted_keys, &b.emitted_keys), ("affected", &a.affected_keys, &b.affected_keys)] {
        assert!(small.is_some() && larger.is_some(), "unobserved {name} key set");
        assert_eq!(small, larger, "{name} key-set growth");
    }
    assert_eq!(a.accepted_keys, b.accepted_keys, "accepted key-set growth");
    if let (Some(small), Some(larger)) = (&a.external_key_scope, &b.external_key_scope) {
        assert_eq!(small.attempted_keys, larger.attempted_keys, "dispatch key-set growth");
        assert_eq!(small.held_keys, larger.held_keys, "held key-set growth");
        assert_eq!(small.deferred_keys, larger.deferred_keys, "deferred key-set growth");
    } else {
        assert_eq!(a.external_key_scope.is_some(), b.external_key_scope.is_some());
    }
}

fn measurement_gate() -> String {
    assert_eq!(std::env::var("RISUNEST_LWW_VERIFIED_WAVE2_HEAD").ok().as_deref(), Some("verified"),
        "root must release the measurement gate before writing smoke samples");
    let checkpoint = std::env::var("RISUNEST_LWW_MEASUREMENT_CHECKPOINT").expect("verified source checkpoint SHA");
    assert!(checkpoint.len() == 40 && checkpoint.bytes().all(|byte| byte.is_ascii_hexdigit()));
    checkpoint
}

/// The fixed missed edit/publication is excluded. Only recipient reopen and real receive/ACK are timed.
fn final_server_resume_endpoint(source:&mut PersistentStore,sender:&LwwClient,destination:PersistentStore,receiver:LwwClient,
    asset_hashes:&[String],direction:final_runner::Direction,iteration:u32,warmup:bool)
    ->Result<(PersistentStore,LwwClient,final_runner::NativeResumeSample),final_runner::CycleFailure> {
    let root=destination.repository_root().to_owned();
    let stored=destination.server_stored_config().map_err(|e|format!("{e:?}"))?.ok_or("resume lacks stored registration")?;
    let writer=destination.lww_clock_state().map_err(|e|e.to_string())?.writer_id;
    let io=receiver.client.test_io.as_ref().ok_or("resume HTTP observer absent")?.clone();
    drop(receiver);drop(destination);
    let (commit,expected)=prepare_edit(source,SmallChange::Setting,final_direction(direction),iteration.into());
    source.commit(&commit).map_err(|e|e.to_string())?;
    let receipt=server_sync::lww_tests::publish_cycle(sender,source,&[]).map_err(|e|format!("{e:?}"))?
        .ok_or("missed edit emitted no publication")?;
    let body:Vec<u8>=sender.log.0.query_row("SELECT body FROM publications WHERE id=?1",[&receipt.operation_id],|r|r.get(0))
        .map_err(|e|e.to_string())?;
    let request:risunest_sync_wire::lww::PushRequest=serde_json::from_slice(&body).map_err(|e|e.to_string())?;
    if request.operation_id!=receipt.operation_id {return Err("resume missed-operation identity mismatch".into());}
    let missed_published_keys=key_set(request.changes.into_iter().map(|c|c.key));
    io.reset();reset_work(asset_hashes);
    let started=Instant::now();
    let result=(|| {
        let mut destination=PersistentStore::open(&root).map_err(|e|e.to_string())?;
        let mut receiver=LwwClient::new(&root,stored.resolve(&root).map_err(|e|format!("{e:?}"))?)
            .map_err(|e|format!("{e:?}"))?;
        receiver.access=Some(stored);receiver.client.test_io=Some(io.clone());
        let reopened_ms=started.elapsed().as_secs_f64()*1000.0;
        let completion=server_sync::lww_tests::receive_cycle(&receiver,&mut destination,&[]).map_err(|e|format!("{e:?}"))?;
        let receiver_durable_ack_ms=started.elapsed().as_secs_f64()*1000.0;
        Ok::<_,String>((destination,receiver,reopened_ms,receiver_durable_ack_ms,completion))
    })();
    let mut observation=take_work(false);observation.shared_head_reads=Some(0);
    let [requests,sent,received]=io.snapshot();observation.requests=requests;
    observation.uploaded_bytes=sent;observation.downloaded_bytes=received;
    let (destination,receiver,reopened_ms,receiver_durable_ack_ms,completion)=result
        .map_err(|reason|final_runner::CycleFailure {reason,observation:Some(Box::new(observation.clone()))})?;
    verify_post_scope(&observation.clone(),|| {
    if destination.lww_clock_state().map_err(|e|e.to_string())?.writer_id!=writer {return Err("resume changed native writer".into());}
    verify(&destination,SmallChange::Setting,&expected);
    Ok((destination,receiver,final_runner::NativeResumeSample {direction,iteration,warmup,reopened_ms,receiver_durable_ack_ms,
        missed_published_keys,affected_keys:key_set(completion.result.affected_keys),observation}))
    })
}

pub(crate) async fn final_external_resume_endpoint(source:&mut PersistentStore,
    sender:&mut crate::external_storage::lww_engine::ExternalLwwEngine,destination:PersistentStore,
    receiver:crate::external_storage::lww_engine::ExternalLwwEngine,provider:&crate::external_storage::fake::FakeProvider,
    asset_hashes:&[String],direction:final_runner::Direction,iteration:u32,warmup:bool)
    ->Result<(PersistentStore,crate::external_storage::lww_engine::ExternalLwwEngine,final_runner::NativeResumeSample),final_runner::CycleFailure> {
    use crate::external_storage::lww_engine::{ExternalLwwEngine,cycle_keys};
    let root=destination.repository_root().to_owned();
    let writer=destination.lww_clock_state().map_err(|e|e.to_string())?.writer_id;
    let authority=destination.lww_binding_authority().map_err(|e|e.to_string())?;
    let ExternalLwwEngine {provider:engine_provider,repository,library,root_key,admission,connection_id,connection_root,capabilities,descriptor}=receiver;
    drop(destination);
    let (commit,expected)=prepare_edit(source,SmallChange::Setting,final_direction(direction),iteration.into());
    source.commit(&commit).map_err(|e|e.to_string())?;
    cycle_keys::reset();
    sender.publish(source,source.lww_binding_authority().map_err(|e|e.to_string())?,&[],&Cancellation::default())
        .await.map_err(|e|e.to_string())?;
    let publication=cycle_keys::take();
    if !publication.complete() || publication.selected_keys!=publication.emitted_keys || publication.emitted_keys.is_empty() {
        return Err("external resume missed publication did not settle exact keys".into());
    }
    let missed_published_keys=key_set(publication.emitted_keys);
    let requests_before=provider.read_count()+provider.upload_count()+provider.listing_count();
    let bytes_before=provider.transferred_body_bytes();let heads_before=provider.read_attempts("head");
    reset_work(asset_hashes);
    let started=Instant::now();
    let result=async {
        let mut destination=PersistentStore::open(&root).map_err(|e|e.to_string())?;
        let receiver=ExternalLwwEngine {provider:engine_provider,repository,library,root_key,admission,connection_id,connection_root,capabilities,descriptor};
        let reopened_ms=started.elapsed().as_secs_f64()*1000.0;
        receiver.receive_and_apply(&mut destination,authority,&[],&Cancellation::default()).await.map_err(|e|e.to_string())?;
        Ok::<_,String>((destination,receiver,reopened_ms,started.elapsed().as_secs_f64()*1000.0))
    }.await;
    let mut observation=take_work(true);
    let keys=cycle_keys::take();
    let bytes_after=provider.transferred_body_bytes();
    observation.requests=(provider.read_count()+provider.upload_count()+provider.listing_count()-requests_before) as u64;
    observation.uploaded_bytes=bytes_after.0-bytes_before.0;observation.downloaded_bytes=bytes_after.1-bytes_before.1;
    observation.shared_head_reads=Some((provider.read_attempts("head")-heads_before) as u64);
    let (destination,receiver,reopened_ms,receiver_durable_ack_ms)=result
        .map_err(|reason|final_runner::CycleFailure {reason,observation:Some(Box::new(observation.clone()))})?;
    verify_post_scope(&observation.clone(),|| {
        if !keys.complete() || !keys.held_keys.is_empty() || !keys.deferred_keys.is_empty() {
            return Err("external resume receive did not settle all observed keys".into());
        }
        if destination.lww_clock_state().map_err(|e|e.to_string())?.writer_id!=writer
            || destination.lww_binding_authority().map_err(|e|e.to_string())?!=authority {
            return Err("external resume changed writer or binding authority".into());
        }
        verify(&destination,SmallChange::Setting,&expected);verify_settled(&destination,&receiver);
        let sample=final_runner::NativeResumeSample {direction,iteration,warmup,reopened_ms,receiver_durable_ack_ms,
            missed_published_keys,affected_keys:key_set(keys.affected_keys),observation};
        sample.validate()?;
        Ok((destination,receiver,sample))
    })
}

fn paired_diagnostics(a:&NativeSample,b:&NativeSample) -> Value {
    let domains=a.observation.hashes.keys().chain(b.observation.hashes.keys())
        .collect::<std::collections::BTreeSet<_>>();
    let growth=domains.into_iter().filter_map(|domain| {
        let baseline=a.observation.hashes.get(domain).cloned().unwrap_or_default();
        let trial=b.observation.hashes.get(domain).cloned().unwrap_or_default();
        if trial.calls<=baseline.calls && trial.bytes<=baseline.bytes {return None;}
        Some(json!({"domain":domain,"baseline":baseline,"trial":trial}))
    }).collect::<Vec<_>>();
    let mut intent_deltas=Vec::new();
    if let (Some(baseline),Some(trial))=(&a.observation.receive_intents,&b.observation.receive_intents) {
        for (index,(left,right)) in baseline.inputs.iter().zip(&trial.inputs).enumerate() {
            intent_deltas.push(json!({"input_index":index,"baseline_bytes":left.input_bytes,
                "trial_bytes":right.input_bytes,"exclusive_field_deltas":invalid_diagnostics::byte_deltas(left,right)}));
        }
    }
    let mut commit_deltas=Vec::new();
    if let (Some(baseline),Some(trial))=(&a.observation.commit_intents,&b.observation.commit_intents) {
        for (index,(left,right)) in baseline.inputs.iter().zip(&trial.inputs).enumerate() {
            commit_deltas.push(json!({"input_index":index,"baseline_bytes":left.input_bytes,
                "trial_bytes":right.input_bytes,"exclusive_field_deltas":invalid_diagnostics::byte_deltas(left,right)}));
        }
    }
    json!({"iteration":a.iteration,"change":a.change,"direction":a.direction,
        "all_positive_hash_domain_growth":growth,"receive_intent_deltas":intent_deltas,
        "commit_intent_deltas":commit_deltas,
        "baseline_commit_intents":a.observation.commit_intents,"trial_commit_intents":b.observation.commit_intents,
        "baseline_receive_intents":a.observation.receive_intents,"trial_receive_intents":b.observation.receive_intents})
}

fn write_invalid_diagnostics(directory:&Path,by_scale:&[Vec<NativeSample>],reason:&str,index:Option<usize>) {
    std::fs::create_dir(directory).unwrap();
    let comparisons=if by_scale.len()==2 {
        by_scale[0].iter().zip(&by_scale[1]).map(|(a,b)|paired_diagnostics(a,b)).collect::<Vec<_>>()
    } else {Vec::new()};
    let output=std::fs::OpenOptions::new().write(true).create_new(true)
        .open(directory.join("INVALID-native-diagnostics.json")).unwrap();
    serde_json::to_writer_pretty(output,&json!({"schema":"risunest.lww-invalid-diagnostics/v1",
        "status":"INVALID","accepted":false,"failure_reason":reason,"failed_comparison_index":index,
        "checkpoint":std::env::var("RISUNEST_LWW_MEASUREMENT_CHECKPOINT").ok(),
        "field_semantics":"original JSON token spans, exclusive own bytes conserve each actual SHA input; typed roundtrip required",
        "attribution":"transport control is a candidate only; changes, values, keys and framing have no allowance",
        "comparisons":comparisons,"all_post_take_samples":by_scale})).unwrap();
}

#[derive(Serialize)]
struct PairAttribution {
    iteration:u64,
    change:SmallChange,
    direction:Direction,
    proof:native_observation::FrozenGrowthProof,
}

fn validate_smoke_pairs(directory:&Path,by_scale:&[Vec<NativeSample>]) -> Result<Vec<PairAttribution>,String> {
    if by_scale.len()!=2 || by_scale[0].len()!=by_scale[1].len() {
        let reason="incomplete paired sample matrix";
        write_invalid_diagnostics(directory,by_scale,reason,None);
        return Err(reason.into());
    }
    let mut attributions=Vec::new();
    for (index,(a,b)) in by_scale[0].iter().zip(&by_scale[1]).enumerate() {
        if [a,b].iter().any(|sample|!sample.observation.receive_intents.as_ref().is_some_and(|e|e.complete)) {
            let reason="incomplete exact receive-intent input provenance";
            write_invalid_diagnostics(directory,by_scale,reason,Some(index));
            return Err(reason.into());
        }
        if [a,b].iter().any(|sample|!sample.observation.commit_intents.as_ref().is_some_and(|e|e.complete)) {
            let reason="incomplete exact commit-intent input provenance";
            write_invalid_diagnostics(directory,by_scale,reason,Some(index));
            return Err(reason.into());
        }
        let keys=std::panic::catch_unwind(std::panic::AssertUnwindSafe(||assert_same_key_sets(a,b)));
        let result=if keys.is_err() {Err("exact key-set comparison failed".into())}
            else {native_observation::assert_frozen_growth(&a.observation,&b.observation)};
        match result {
            Ok(proof)=>attributions.push(PairAttribution {iteration:a.iteration,change:a.change,direction:a.direction,proof}),
            Err(error)=>{
                write_invalid_diagnostics(directory,by_scale,&error,Some(index));
                return Err(error);
            }
        }
    }
    Ok(attributions)
}

fn write_samples(directory:&Path, samples:&[NativeSample],attributions:&[PairAttribution]) {
    let checkpoint = measurement_gate();
    assert!(!samples.is_empty());
    assert_eq!(attributions.len()*2,samples.len());
    for sample in samples {
        sample.observation.validate_routine_invariants().unwrap();
        assert!(sample.observation.receive_intents.as_ref().is_some_and(|evidence|evidence.complete),
            "receive-intent input provenance must be complete before accepted output");
        assert!(sample.observation.commit_intents.as_ref().is_some_and(|evidence|evidence.complete),
            "commit-intent input provenance must be complete before accepted output");
        assert!(sample.selected_keys.is_some() && sample.emitted_keys.is_some() && sample.affected_keys.is_some(),
            "key-set observation must be linked before smoke output");
        if let Some(keys) = &sample.external_key_scope {
            assert_eq!(keys.failed_operations, 0);
            assert_eq!(keys.pending_operations, 0);
        }
    }
    std::fs::create_dir(directory).unwrap();
    serde_json::to_writer_pretty(File::create(directory.join("native-smoke.json")).unwrap(),
        &json!({"schema":"risunest-lww-native-smoke/v1", "scope":"native-content-identity-sha-inputs",
            "verified_checkpoint":checkpoint, "timing_origin":"native commit start; cumulative milliseconds",
            "bootstrap_phase":"excluded; both endpoints settled before warmup or sample counters",
            "capture_work_scope":"partial native message capture work; SHA totals include separately observed full content identity inputs",
            "body_work_semantics":{
                "domains":"all-purpose actual CAS boundary totals, not summed again with object or role work",
                "asset":"actual asset-purpose object IO, including mixed asset/control objects",
                "control":"exclusively actual control-purpose object IO",
                "unknown":"unclassified canonical object IO; invalidates samples",
                "owned":"exclusive Control identity-metadata opens only: per-object/domain attempts, successes and verified proofs equal; zero reads/errors/escapes/pending work; all contributions conserved",
                "inventory":"actual fixture asset aliases checked against persisted catalog during excluded setup, re-registered after every reset"},
            "key_semantics":{
                "local_http_emitted":"immutable body submitted to /push, including dominated submissions",
                "local_http_accepted":"winning server keys",
                "external_emitted":"authenticated publication changes after exact native ACK",
                "external_attempted":"keys at actual segment create dispatch",
                "affected":"actual native ApplyResult after durable completion"},
            "excluded":["server internal SHA", "MAC", "HKDF", "general hashing", "renderer paint", "scheduler empty polls"],
            "precise_control_attribution":attributions,"samples":samples})).unwrap();
}

pub(crate) fn append_native_shard(store:&mut PersistentStore, shard:&native_shards::NativeShard, seed:u64) {
    use crate::asset_repository::PayloadCas;
    use crate::asset_repository::owner_manifest_codec::{encode_owner_manifest,OwnerManifestEntry};
    let cas = PayloadCas::new(store.repository_root()).unwrap();
    let mut registrations = Vec::new();
    let mut aliases = Vec::new();
    for asset in &shard.assets {
        let object = cas.prepare_bytes(&fixture::asset_body(seed,asset.index)).unwrap();
        assert_eq!(object.content_hash,asset.payload_hash);
        assert_eq!(object.byte_size,asset.byte_length);
        registrations.push(super::asset_object_catalog::AssetObjectRegistration {
            object_hash:object.content_hash.clone(),byte_size:object.byte_size});
        aliases.push(super::AssetAlias {key:asset.logical_key.clone(),object_hash:Some(object.content_hash),
            kind:"asset".into(),size:asset.byte_length as i64,mime:"application/octet-stream".into(),
            name:format!("Synthetic asset {}",asset.index),ext:"bin".into(),inlay_type:None,
            width:None,height:None,metadata:json!({})});
    }
    let entries = shard.owner_entries.iter().map(|entry| OwnerManifestEntry {
        tuple:entry.tuple.clone(),payload_hash:Some(hex::decode(&entry.payload_hash).unwrap().try_into().unwrap())
    }).collect::<Vec<_>>();
    let manifest = cas.prepare_bytes(&encode_owner_manifest(&entries).unwrap()).unwrap();
    registrations.push(super::asset_object_catalog::AssetObjectRegistration {
        object_hash:manifest.content_hash.clone(),byte_size:manifest.byte_size});
    store.asset_object_catalog().register(&registrations,1).unwrap();
    let owner = super::AssetOwnerLocator::CharacterAdditionalAssets {
        character_id:shard.character["chaId"].as_str().unwrap().into()};
    let commit = WorkingSetCommit {expected_revision:store.revision().unwrap(),
        add_character:Some(shard.character.clone()),asset_owner_heads:Some(vec![super::AssetOwnerHead::present(
            owner,manifest.content_hash,entries.len() as i64)]),..Default::default()};
    store.commit_with_asset_aliases(&commit,&aliases).unwrap();
}

/// This excluded delta is reported alongside the unchanged base certificate.
pub(crate) fn prepare_large_asset_delta(store:&mut PersistentStore,base:&scale_certificate::ScaleCertificate,
    sender:&LwwClient)->Result<Value,String> {
    use crate::asset_repository::{PayloadCas,owner_manifest_codec::{encode_owner_manifest,decode_owner_manifest,OwnerManifestEntry}};
    base.validate()?;
    let before=store.list_asset_aliases(None).map_err(|e|e.to_string())?.value;
    let base_hashes=base.assets.iter().map(|asset|asset.payload_hash.clone()).collect::<std::collections::BTreeSet<_>>();
    let before_hashes=before.iter().filter_map(|alias|alias.object_hash.clone()).collect::<std::collections::BTreeSet<_>>();
    if before.len()!=base.assets.len() || before_hashes!=base_hashes {
        return Err("large Asset delta requires the exact certified base alias inventory".into());
    }
    let cas=PayloadCas::new(store.repository_root()).map_err(|e|e.to_string())?;
    let bytes=(0..96*1024u32).map(|index|((index.wrapping_mul(73)^index.rotate_left(11)^0x5b)&255) as u8).collect::<Vec<_>>();
    let payload=cas.prepare_bytes(&bytes).map_err(|e|e.to_string())?;
    if base_hashes.contains(&payload.content_hash) {return Err("large delta collides with certified base Asset".into());}
    let owner=super::AssetOwnerLocator::CharacterAdditionalAssets {character_id:"synthetic-character-0".into()};
    let previous=store.read_asset_owner_head(&owner,None).map_err(|e|e.to_string())?.ok_or("selected fixture owner absent")?.value;
    let previous_manifest=previous.manifest_hash.ok_or("selected fixture owner manifest absent")?;
    let encoded=cas.read_object(&previous_manifest).map_err(|e|e.to_string())?.ok_or("selected owner manifest body absent")?;
    let mut entries=decode_owner_manifest(&encoded).map_err(|e|e.to_string())?;
    let key="assets/synthetic-transfer-large.bin";
    entries.push(OwnerManifestEntry {tuple:["Synthetic transfer Asset".into(),key.into(),"bin".into()],
        payload_hash:Some(hex::decode(&payload.content_hash).map_err(|e|e.to_string())?.try_into().map_err(|_|"invalid actual payload hash")?)});
    let manifest=cas.prepare_bytes(&encode_owner_manifest(&entries).map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;
    store.asset_object_catalog().register(&[
        super::asset_object_catalog::AssetObjectRegistration {object_hash:payload.content_hash.clone(),byte_size:payload.byte_size},
        super::asset_object_catalog::AssetObjectRegistration {object_hash:manifest.content_hash.clone(),byte_size:manifest.byte_size}],1).map_err(|e|e.to_string())?;
    let mut character=store.read_character("synthetic-character-0",None).map_err(|e|e.to_string())?.ok_or("selected fixture parent absent")?.value;
    character["additionalAssets"]=json!(entries.iter().map(|entry|entry.tuple.clone()).collect::<Vec<_>>());
    let alias=super::AssetAlias {key:key.into(),object_hash:Some(payload.content_hash.clone()),kind:"asset".into(),
        size:payload.byte_size as i64,mime:"application/octet-stream".into(),name:"Synthetic transfer Asset".into(),ext:"bin".into(),
        inlay_type:None,width:None,height:None,metadata:json!({})};
    store.commit_with_asset_aliases(&WorkingSetCommit {expected_revision:store.revision().map_err(|e|e.to_string())?,
        character:Some(character),asset_owner_heads:Some(vec![super::AssetOwnerHead::present(owner.clone(),manifest.content_hash.clone(),entries.len() as i64)]),
        ..Default::default()},std::slice::from_ref(&alias)).map_err(|e|e.to_string())?;
    server_sync::lww_tests::drain_publications(sender,store,&[]).map_err(|e|format!("{e:?}"))?;
    let path=cas.object_path(&payload.content_hash).map_err(|e|e.to_string())?.ok_or("large delta physical file absent")?;
    let (metadata,sparse,reparse)=observed_file_kind(&path)?;
    if !metadata.is_file() || sparse || reparse || metadata.len()!=payload.byte_size || metadata.len()<=64*1024 {
        return Err("large delta actual materialized body proof failed".into());
    }
    let read=cas.read_object(&payload.content_hash).map_err(|e|e.to_string())?.ok_or("large delta body absent")?;
    if risunest_sync_wire::hash(&read)!=payload.content_hash {return Err("large delta physical digest mismatch".into());}
    let catalog_size:i64=store.connection.query_row("SELECT byte_size FROM asset_objects WHERE object_hash=?1",
        [&payload.content_hash],|row|row.get(0)).map_err(|e|e.to_string())?;
    if catalog_size!=payload.byte_size as i64
        || !store.lww_read_outbox(store.lww_binding_authority().map_err(|e|e.to_string())?,1)
            .map_err(|e|e.to_string())?.entries.is_empty() {
        return Err("large delta actual catalog or publication settlement proof differs".into());
    }
    let after=store.list_asset_aliases(None).map_err(|e|e.to_string())?.value;
    let after_hashes=after.iter().filter_map(|alias|alias.object_hash.clone()).collect::<std::collections::BTreeSet<_>>();
    if after.len()!=before.len()+1 || after_hashes.len()!=base_hashes.len()+1
        || store.read_asset_owner_head(&owner,None).map_err(|e|e.to_string())?.ok_or("delta owner missing after publication")?.value.manifest_hash.as_deref()!=Some(manifest.content_hash.as_str()) {
        return Err("large delta owner and unique current inventory proof differs".into());
    }
    Ok(json!({"phase":"excluded-certified-base-plus-explicit-delta","baseCertificateIsPostSetup":false,
        "baseOwnedAssetCount":base.assets.len(),"currentUniqueOwnedAssetCount":after_hashes.len(),
        "payloadHash":payload.content_hash,"byteSize":payload.byte_size,"ownerCharacterId":"synthetic-character-0",
        "ownerManifestHash":manifest.content_hash,"ownerEntryCount":entries.len(),"physicalRegularFile":true,
        "catalogByteSize":catalog_size,"normalPublicationDrained":true,"sourcePhysicalReadProof":null,"currentAssetHashes":after_hashes}))
}

#[derive(Debug,Serialize)]
pub(crate) struct NativeBuildProgress {
    pub shards:u64,
    pub assigned_assets:u64,
    pub generated_input_bytes:u64,
    pub maximum_shard_bytes:u64,
    pub observed_active_record_bytes:u64,
}

#[derive(Debug,Serialize)]
pub(crate) struct BuiltNativeFixture {
    pub progress:NativeBuildProgress,
    pub certificate:scale_certificate::ScaleCertificate,
}

#[derive(Debug,Serialize)]
pub(crate) struct ConstructedNativeTarget {
    pub root:String,
    pub writer_id:String,
    pub configuration:fixture_configuration::NativeFixtureConfiguration,
    pub fixture:BuiltNativeFixture,
}

pub(crate) fn construct_native_target(root:&Path,requirement:scale_certificate::ScaleRequirement,
    configuration:fixture_configuration::NativeFixtureConfiguration,
    progress:impl FnMut(&NativeBuildProgress)->Result<(),String>)->Result<ConstructedNativeTarget,String> {
    if !root.is_absolute() {return Err("synthetic native root must be absolute".into());}
    configuration.initial_values()?;
    if root.exists() {
        let (metadata,sparse,reparse)=observed_file_kind(root)?;
        if !metadata.is_dir() || sparse || reparse || std::fs::read_dir(root).map_err(|e|e.to_string())?.next().is_some() {
            return Err("synthetic native construction requires an empty regular directory".into());
        }
    }
    let mut store=PersistentStore::open(root).map_err(|e|e.to_string())?;
    let plan=native_shards::ShardPlan::new(1,requirement.minimums().1);
    let fixture=build_native_fixture_configured(&mut store,plan,requirement,&configuration,progress)?;
    let writer_id=store.lww_clock_state().map_err(|e|e.to_string())?.writer_id;
    Ok(ConstructedNativeTarget {root:root.to_str().ok_or("synthetic root is not UTF-8")?.to_owned(),writer_id,configuration,fixture})
}

#[test]
#[ignore = "final-stage synthetic construction, explicit fresh root and verified checkpoint required"]
fn construct_native_target_for_final_stage() {
    assert_eq!(std::env::var("RISUNEST_LWW_FINAL_STAGE").as_deref(),Ok("authorized-synthetic"));
    let checkpoint=std::env::var("RISUNEST_LWW_FINAL_CHECKPOINT").expect("verified final checkpoint");
    assert!(checkpoint.len()==40 && checkpoint.bytes().all(|b|b.is_ascii_hexdigit()));
    let root=std::path::PathBuf::from(std::env::var("RISUNEST_LWW_NATIVE_ROOT").expect("fresh synthetic native root"));
    let output=std::path::PathBuf::from(std::env::var("RISUNEST_LWW_OUTPUT_DIRECTORY").expect("separate structural output directory"));
    assert!(!root.starts_with(&output) && !output.starts_with(&root),"native data and output roots must be separate");
    std::fs::create_dir_all(&output).unwrap();
    assert!(!output.join("native-construction.json").exists() && !output.join("INVALID-native-construction.json").exists());
    let requirement=match std::env::var("RISUNEST_LWW_FINAL_TIER").as_deref() {
        Ok("target")=>scale_certificate::ScaleRequirement::Target,
        Ok("above-target")=>scale_certificate::ScaleRequirement::AboveTarget,
        _=>panic!("exact final target/above-target tier required"),
    };
    let local_sse=match std::env::var("RISUNEST_LWW_LOCAL_SSE_ADDRESS") {
        Ok(address)=>Some(fixture_configuration::LocalSseEndpoint {address:address.parse().expect("local IP address"),
            port:std::env::var("RISUNEST_LWW_LOCAL_SSE_PORT").expect("local SSE port").parse().unwrap()}),
        Err(std::env::VarError::NotPresent)=>None,
        Err(error)=>panic!("invalid local SSE address: {error}"),
    };
    let built=construct_native_target(&root,requirement,fixture_configuration::NativeFixtureConfiguration {local_sse},|progress| {
        if progress.shards%100==0 {eprintln!("M-NATIVE-CONSTRUCTION-PROGRESS {}",serde_json::to_string(progress).unwrap());}
        Ok(())
    });
    let built=match built {
        Ok(built)=>built,
        Err(reason)=>{
            let file=std::fs::OpenOptions::new().create_new(true).write(true).open(output.join("INVALID-native-construction.json")).unwrap();
            serde_json::to_writer_pretty(file,&json!({"status":"INVALID","checkpoint":checkpoint,"reason":reason})).unwrap();
            panic!("synthetic construction failed; only INVALID structural output written");
        },
    };
    use sha2::Digest;
    let executable=std::env::current_exe().unwrap();
    let fingerprint=hex::encode(sha2::Sha256::digest(std::fs::read(executable).unwrap()));
    let file=std::fs::OpenOptions::new().create_new(true).write(true).open(output.join("native-construction.json")).unwrap();
    serde_json::to_writer_pretty(file,&json!({"schema":"risunest.lww-native-construction/v1","checkpoint":checkpoint,
        "binary_sha256":fingerprint,"setup_only":true,"target":built})).unwrap();
}

#[test]
#[ignore = "setup-only populated renderer endpoint; explicit coordinator authorization required"]
fn populated_renderer_source_for_final_stage() {
    final_matrix::renderer_setup().expect("renderer source setup retained its structural cleanup result");
}

#[test]
#[ignore = "final-stage physical target matrix, explicit coordinator authorization required"]
fn final_native_matrix_for_final_stage() {
    final_matrix::run().expect("final matrix retained its raw failure output");
}

pub(crate) fn build_final_native_tiers(target:&mut PersistentStore,above:&mut PersistentStore,
    mut progress:impl FnMut(scale_certificate::ScaleRequirement,&NativeBuildProgress)->Result<(),String>)
    ->Result<[BuiltNativeFixture;2],String> {
    use scale_certificate::ScaleRequirement;
    let target_plan=native_shards::ShardPlan::new(1,ScaleRequirement::Target.minimums().1);
    let above_plan=native_shards::ShardPlan::new(1,ScaleRequirement::AboveTarget.minimums().1);
    let base=build_native_fixture(target,target_plan,ScaleRequirement::Target,|state|progress(ScaleRequirement::Target,state))?;
    let larger=build_native_fixture(above,above_plan,ScaleRequirement::AboveTarget,|state|progress(ScaleRequirement::AboveTarget,state))?;
    scale_certificate::validate_above_target(&base.certificate,&larger.certificate)?;
    Ok([base,larger])
}

fn sql_u64(row:&rusqlite::Row<'_>,column:usize) -> rusqlite::Result<u64> {
    let value:i64=row.get(column)?;
    u64::try_from(value).map_err(|error|rusqlite::Error::FromSqlConversionFailure(
        column,rusqlite::types::Type::Integer,Box::new(error)))
}

pub(crate) fn build_native_fixture<F>(store:&mut PersistentStore,plan:native_shards::ShardPlan,
    requirement:scale_certificate::ScaleRequirement,mut progress:F) -> Result<BuiltNativeFixture,String>
where F:FnMut(&NativeBuildProgress)->Result<(),String> {
    build_native_fixture_configured(store,plan,requirement,&fixture_configuration::NativeFixtureConfiguration::default(),&mut progress)
}

pub(crate) fn build_native_fixture_configured<F>(store:&mut PersistentStore,plan:native_shards::ShardPlan,
    requirement:scale_certificate::ScaleRequirement,configuration:&fixture_configuration::NativeFixtureConfiguration,mut progress:F)
    ->Result<BuiltNativeFixture,String> where F:FnMut(&NativeBuildProgress)->Result<(),String> {
    let (root,presets)=configuration.initial_values()?;
    let (minimum_bytes,minimum_assets)=requirement.minimums();
    if plan.assets<minimum_assets {return Err("shard plan cannot satisfy required asset count".into());}
    let generation=super::active_generation(&store.connection).map_err(|e|e.to_string())?;
    let characters:u64=store.connection.query_row("SELECT COUNT(*) FROM characters WHERE generation=?1",
        [&generation],|row|sql_u64(row,0)).map_err(|e|e.to_string())?;
    if characters!=0 || !store.list_asset_aliases(None).map_err(|e|e.to_string())?.value.is_empty()
        || store.lww_binding_state().map_err(|e|e.to_string())?.target!=SyncTarget::None {
        return Err("native fixture construction requires a fresh unbound synthetic store".into());
    }
    let revision=store.revision().map_err(|e|e.to_string())?;
    let staging=store.replace_begin().map_err(|e|e.to_string())?.staging_id;
    store.replace_put_root(&staging,&root).map_err(|e|e.to_string())?;
    store.replace_put_presets(&staging,&presets).map_err(|e|e.to_string())?;
    store.replace_commit(&staging,Some(revision)).map_err(|e|e.to_string())?;
    let mut shards=native_shards::NativeShards::new(plan.clone())?;
    let mut state=NativeBuildProgress {shards:0,assigned_assets:0,generated_input_bytes:0,
        maximum_shard_bytes:0,observed_active_record_bytes:0};
    loop {
        let mut shard=shards.next().ok_or("native shard ordinals exhausted")??;
        if configuration.local_sse.is_some() {
            configuration.configure_character(&mut shard.character);
            let bytes=serde_json::to_vec(&shard.character).map_err(|e|e.to_string())?;
            if bytes.len()>native_shards::MAX_SHARD_BYTES {return Err("configured native shard exceeds bounded bytes".into());}
            shard.serialized_character_bytes=bytes.len() as u64;
            use sha2::Digest;
            shard.character_sha256=hex::encode(sha2::Sha256::digest(&bytes));
        }
        append_native_shard(store,&shard,plan.seed);
        state.shards=shards.generated_characters();state.assigned_assets=shards.assigned_assets();
        state.generated_input_bytes=state.generated_input_bytes.checked_add(shard.serialized_character_bytes).ok_or("input size overflow")?;
        state.maximum_shard_bytes=state.maximum_shard_bytes.max(shard.serialized_character_bytes);
        let generation=super::active_generation(&store.connection).map_err(|e|e.to_string())?;
        let character=shard.character["chaId"].as_str().unwrap();
        let records:u64=store.connection.query_row(
            "SELECT COALESCE(SUM(length(CAST(value AS BLOB))),0) FROM messages WHERE generation=?1 AND character_id=?2",
            rusqlite::params![generation,character],|row|sql_u64(row,0)).map_err(|e|e.to_string())?;
        let detail:u64=store.connection.query_row(
            "SELECT length(CAST(detail AS BLOB)) FROM characters WHERE generation=?1 AND character_id=?2",
            rusqlite::params![generation,character],|row|sql_u64(row,0)).map_err(|e|e.to_string())?;
        state.observed_active_record_bytes=state.observed_active_record_bytes.checked_add(records)
            .and_then(|bytes|bytes.checked_add(detail)).ok_or("observed record size overflow")?;
        progress(&state)?;
        if state.shards<2 || state.assigned_assets!=plan.assets || state.observed_active_record_bytes<minimum_bytes {continue;}
        store.checkpoint(super::CheckpointMode::Truncate).map_err(|e|e.to_string())?;
        let pages:u64=store.connection.query_row("PRAGMA page_count",[],|row|sql_u64(row,0)).map_err(|e|e.to_string())?;
        let free:u64=store.connection.query_row("PRAGMA freelist_count",[],|row|sql_u64(row,0)).map_err(|e|e.to_string())?;
        let page_size:u64=store.connection.query_row("PRAGMA page_size",[],|row|sql_u64(row,0)).map_err(|e|e.to_string())?;
        if pages.checked_sub(free).and_then(|pages|pages.checked_mul(page_size)).ok_or("invalid live page allocation")?<minimum_bytes {continue;}
        let certificate=collect_native_scale_certificate(store,&plan,requirement)?;
        certificate.validate()?;
        if certificate.database.active_record_bytes!=state.observed_active_record_bytes {
            return Err("observed shard records differ from complete active native record inventory".into());
        }
        return Ok(BuiltNativeFixture {progress:state,certificate});
    }
}

fn observed_file_kind(path:&Path) -> Result<(std::fs::Metadata,bool,bool),String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error|error.to_string())?;
    #[cfg(windows)]
    let (sparse,reparse) = {
        use std::os::windows::fs::MetadataExt;
        (metadata.file_attributes() & 0x200 != 0,metadata.file_attributes() & 0x400 != 0)
    };
    #[cfg(unix)]
    let (sparse,reparse) = {
        use std::os::unix::fs::MetadataExt;
        (metadata.blocks().saturating_mul(512)<metadata.len(),metadata.file_type().is_symlink())
    };
    #[cfg(not(any(windows,unix)))]
    return Err("file allocation observer unsupported on this target".into());
    #[cfg(any(windows,unix))]
    Ok((metadata,sparse,reparse))
}

pub(crate) fn collect_native_scale_certificate(store:&mut PersistentStore, plan:&native_shards::ShardPlan,
    requirement:scale_certificate::ScaleRequirement) -> Result<scale_certificate::ScaleCertificate,String> {
    use scale_certificate::{CertifiedAsset,DatabaseAllocation,ScaleCertificate};
    use sha2::{Digest,Sha256};
    store.checkpoint(super::CheckpointMode::Truncate).map_err(|e|e.to_string())?;
    let mut reopened=PersistentStore::open(store.repository_root()).map_err(|e|e.to_string())?;
    reopened.checkpoint(super::CheckpointMode::Truncate).map_err(|e|e.to_string())?;
    let store=&mut reopened;
    let checked = || -> Result<ScaleCertificate,String> {
        let connection = &store.connection;
        let scalar = |sql:&str| connection.query_row(sql,[],|row|sql_u64(row,0)).map_err(|e|e.to_string());
        let generation = super::active_generation(connection).map_err(|e|e.to_string())?;
        let active_scalar = |sql:&str| connection.query_row(sql,[&generation],|row|sql_u64(row,0)).map_err(|e|e.to_string());
        let db_path = Path::new(connection.path().ok_or("native SQLite path missing")?);
        let (metadata,sparse,reparse) = observed_file_kind(db_path)?;
        let wal_path = db_path.with_file_name(format!("{}-wal",db_path.file_name().unwrap().to_str().unwrap()));
        let wal_bytes = match std::fs::metadata(wal_path) {
            Ok(value)=>value.len(),Err(error) if error.kind()==std::io::ErrorKind::NotFound=>0,
            Err(error)=>return Err(error.to_string()),
        };
        let schema = |db:&rusqlite::Connection| -> Result<Vec<(String,String,Option<String>)>,String> {
            let mut statement = db.prepare("SELECT type,name,sql FROM sqlite_master ORDER BY type,name")
                .map_err(|e|e.to_string())?;
            let rows = statement.query_map([],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?)))
                .map_err(|e|e.to_string())?;
            rows.collect::<Result<Vec<_>,_>>().map_err(|e|e.to_string())
        };
        let template_root = tempfile::tempdir().map_err(|e|e.to_string())?;
        let template = PersistentStore::open(template_root.path()).map_err(|e|e.to_string())?;
        let integrity:String = connection.query_row("PRAGMA integrity_check",[],|row|row.get(0)).map_err(|e|e.to_string())?;
        let database = DatabaseAllocation {page_size:scalar("PRAGMA page_size")?,page_count:scalar("PRAGMA page_count")?,
            freelist_count:scalar("PRAGMA freelist_count")?,file_bytes:metadata.len(),wal_bytes,
            regular_file:metadata.is_file(),sparse_file:sparse,reparse_point:reparse,checkpoint_complete:true,reopened:true,
            integrity_checked:integrity=="ok",native_schema_only:schema(connection)?==schema(&template.connection)?,
            active_characters:active_scalar("SELECT COUNT(*) FROM characters WHERE generation=?1")?,
            active_conversations:active_scalar("SELECT COUNT(*) FROM conversations WHERE generation=?1")?,
            active_messages:active_scalar("SELECT COUNT(*) FROM messages WHERE generation=?1")?,
            active_record_bytes:active_scalar("SELECT COALESCE(SUM(length(CAST(value AS BLOB))),0) FROM messages WHERE generation=?1")?
                .checked_add(active_scalar("SELECT COALESCE(SUM(length(CAST(detail AS BLOB))),0) FROM characters WHERE generation=?1")?)
                .ok_or("native record byte overflow")?,
            maximum_message_bytes:active_scalar("SELECT COALESCE(MAX(length(CAST(value AS BLOB))),0) FROM messages WHERE generation=?1")?,
            maximum_character_detail_bytes:active_scalar("SELECT COALESCE(MAX(length(CAST(detail AS BLOB))),0) FROM characters WHERE generation=?1")?,
            maximum_root_bytes:active_scalar("SELECT COALESCE(MAX(length(CAST(value AS BLOB))),0) FROM root WHERE generation=?1")?};
        let aliases = store.list_asset_aliases(None).map_err(|e|e.to_string())?.value;
        if aliases.len() as u64 != plan.assets {return Err("actual fixture alias count differs from shard plan".into());}
        let aliases_by_key = aliases.iter().map(|alias|(alias.key.as_str(),alias)).collect::<std::collections::BTreeMap<_,_>>();
        let cas = crate::asset_repository::PayloadCas::new(store.repository_root()).map_err(|e|e.to_string())?;
        let mut owner_references = std::collections::BTreeMap::<String,u64>::new();
        let mut owner_manifests=Vec::new();
        let mut verified_owner_heads = 0;
        for head in store.list_asset_owner_heads(None).map_err(|e|e.to_string())?.value {
            if !head.present {continue;}
            let super::AssetOwnerLocator::CharacterAdditionalAssets {character_id} = &head.owner else {
                return Err("unexpected owner kind in synthetic shard fixture".into());
            };
            let hash = head.manifest_hash.as_deref().ok_or("owner manifest identity missing")?;
            let path=cas.object_path(hash).map_err(|e|e.to_string())?.ok_or("owner manifest physical body missing")?;
            let (metadata,sparse,reparse)=observed_file_kind(&path)?;
            if !metadata.is_file() || sparse || reparse || metadata.len()>1024*1024 {
                return Err("owner manifest is indirect, sparse or unbounded".into());
            }
            let bytes = cas.read_object(hash).map_err(|e|e.to_string())?.ok_or("owner manifest body missing")?;
            if hex::encode(Sha256::digest(&bytes))!=hash {return Err("owner manifest physical digest mismatch".into());}
            owner_manifests.push((hash.to_owned(),bytes.len() as u64));
            let entries = crate::asset_repository::owner_manifest_codec::decode_owner_manifest(&bytes).map_err(|e|e.to_string())?;
            if entries.len() as i64 != head.entry_count {return Err("owner entry count differs from manifest".into());}
            let character = store.read_character(character_id,None).map_err(|e|e.to_string())?
                .ok_or("owner parent missing")?.value;
            let tuples = json!(entries.iter().map(|entry|entry.tuple.clone()).collect::<Vec<_>>());
            if character["additionalAssets"] != tuples {return Err("owner manifest differs from native parent tuples".into());}
            for entry in entries {
                let hash = hex::encode(entry.payload_hash.ok_or("fixture owner has unresolved payload")?);
                if aliases_by_key.get(entry.tuple[1].as_str()).and_then(|alias|alias.object_hash.as_deref())!=Some(hash.as_str()) {
                    return Err("owner payload does not match actual alias".into());
                }
                *owner_references.entry(hash).or_default()+=1;
            }
            verified_owner_heads+=1;
        }
        let mut catalog = std::collections::BTreeMap::new();
        let mut cursor = None;
        loop {
            let page = store.query_asset_object_catalog(4096,cursor.as_deref()).map_err(|e|e.to_string())?;
            catalog.extend(page.items.into_iter().map(|item|(item.object_hash,item.byte_size)));
            cursor=page.next_cursor;
            if cursor.is_none() {break;}
        }
        let mut assets = Vec::new();
        for (hash,size) in owner_manifests {
            if catalog.get(&hash)!=Some(&size) {return Err("owner manifest physical body/catalog mismatch".into());}
        }
        for alias in aliases {
            let index = alias.key.strip_prefix("assets/synthetic-").and_then(|v|v.strip_suffix(".bin"))
                .and_then(|v|v.parse::<u64>().ok()).ok_or("unexpected synthetic asset alias")?;
            if index>=plan.assets {return Err("asset ordinal outside shard plan".into());}
            let expected = fixture::asset_descriptor(plan.seed,index);
            let hash = alias.object_hash.as_deref().ok_or("fixture alias lacks payload identity")?;
            if hash!=expected.payload_hash || alias.kind!="asset" || alias.size!=expected.byte_length as i64 {
                return Err("fixture alias differs from deterministic body identity".into());
            }
            let path = cas.object_path(hash).map_err(|e|e.to_string())?.ok_or("physical fixture body missing")?;
            let (metadata,sparse,reparse)=observed_file_kind(&path)?;
            if !metadata.is_file() || sparse || reparse || metadata.len()!=expected.byte_length {
                return Err("physical fixture body is indirect, sparse or has incorrect size".into());
            }
            let verified_file_hash=hex::encode(Sha256::digest(std::fs::read(path).map_err(|e|e.to_string())?));
            assets.push(CertifiedAsset {payload_hash:hash.into(),catalog_bytes:*catalog.get(hash).ok_or("payload catalog row missing")?,
                file_bytes:metadata.len(),regular_file:true,sparse_file:sparse,reparse_point:reparse,verified_file_hash,
                active_aliases:1,verified_owner_references:owner_references.get(hash).copied().unwrap_or(0)});
        }
        Ok(ScaleCertificate {schema:"risunest.native-persisted-scale/v1".into(),requirement,database,
            catalog_rows:catalog.len() as u64,verified_owner_heads,assets,
            measured_conversation_sha256:hex::encode(Sha256::digest(serde_json::to_vec(
                &store.read_conversation("synthetic-character-0","synthetic-conversation-0",None)
                    .map_err(|e|e.to_string())?.ok_or("measured conversation missing")?.value)
                    .map_err(|e|e.to_string())?))})
    };
    checked()
}

#[test]
#[ignore = "measurement harness self-test"]
fn bounded_native_shards_have_real_catalog_bodies_and_owner_closure() {
    let (_directory,mut store) = server_sync::lww_tests::local();
    let plan = native_shards::ShardPlan {ordinary_messages:2,assets_per_owner:2,
        ..native_shards::ShardPlan::new(11,3)};
    let mut shards = native_shards::NativeShards::new(plan.clone()).unwrap();
    for _ in 0..2 {append_native_shard(&mut store,&shards.next().unwrap().unwrap(),plan.seed);}
    let certificate = collect_native_scale_certificate(&mut store,&plan,
        scale_certificate::ScaleRequirement::Correctness {minimum_database_bytes:4096,minimum_assets:3}).unwrap();
    assert_eq!(certificate.validate().unwrap().unique_materialized_owned_assets,3);
    assert_eq!(certificate.verified_owner_heads,2);
    assert_eq!(certificate.database.active_messages,4098);
    let mut fake_target=certificate.clone(); fake_target.requirement=scale_certificate::ScaleRequirement::Target;
    assert!(fake_target.validate().is_err());
    store.connection.execute_batch("CREATE TABLE synthetic_padding (value BLOB)").unwrap();
    let padded = collect_native_scale_certificate(&mut store,&plan,certificate.requirement).unwrap();
    assert!(!padded.database.native_schema_only);
    assert!(padded.validate().is_err());
}

#[test]
#[ignore = "measurement harness self-test"]
fn lazy_native_constructor_stops_on_observed_persistence_and_honors_callback_cancellation() {
    let (_directory,mut store)=server_sync::lww_tests::local();
    let plan=native_shards::ShardPlan {ordinary_messages:2,assets_per_owner:2,..native_shards::ShardPlan::new(11,3)};
    let requirement=scale_certificate::ScaleRequirement::Correctness {minimum_database_bytes:4096,minimum_assets:3};
    let built=build_native_fixture(&mut store,plan.clone(),requirement,|progress| {
        assert!(progress.maximum_shard_bytes<=native_shards::MAX_SHARD_BYTES as u64);Ok(())
    }).unwrap();
    assert_eq!(built.progress.shards,2);
    assert_eq!(built.certificate.validate().unwrap().unique_materialized_owned_assets,3);
    assert_eq!(built.certificate.database.active_messages,4098);
    assert!(built.certificate.database.reopened);
    assert_eq!(store.read_root(None).unwrap().value["selectedPersona"],"synthetic-persona-0");
    let (_cancelled_directory,mut cancelled)=server_sync::lww_tests::local();
    let result=build_native_fixture(&mut cancelled,plan,requirement,|_|Err("cancelled synthetic construction".into()));
    assert_eq!(result.err().unwrap(),"cancelled synthetic construction");
}

#[test]
#[ignore = "measurement harness self-test"]
fn large_asset_delta_keeps_base_certificate_and_full_prior_conversation() {
    let server=LocalServerFixture::new();
    let (_directory,mut store)=server_sync::lww_tests::local();
    let plan=native_shards::ShardPlan {ordinary_messages:2,assets_per_owner:2,..native_shards::ShardPlan::new(11,3)};
    let base=build_native_fixture(&mut store,plan,scale_certificate::ScaleRequirement::Correctness {
        minimum_database_bytes:4096,minimum_assets:3},|_|Ok(())).unwrap();
    let sender=server.client(&store);let config=sender.client.config();
    bind(&mut store,SyncTarget::Server(config.endpoint.clone()),&config.endpoint,&config.library_id);
    let prior=store.read_conversation("synthetic-character-0","synthetic-conversation-0",None).unwrap().unwrap().value;
    let receipt=prepare_large_asset_delta(&mut store,&base.certificate,&sender).unwrap();
    assert_eq!(receipt["baseCertificateIsPostSetup"],false);
    assert_eq!(receipt["baseOwnedAssetCount"],3);assert_eq!(receipt["currentUniqueOwnedAssetCount"],4);
    assert_eq!(receipt["byteSize"],96*1024);assert_eq!(receipt["sourcePhysicalReadProof"],Value::Null);
    assert_eq!(base.certificate.validate().unwrap().unique_materialized_owned_assets,3);
    assert_eq!(store.read_conversation("synthetic-character-0","synthetic-conversation-0",None).unwrap().unwrap().value,prior);
    assert!(prepare_large_asset_delta(&mut store,&base.certificate,&sender).unwrap_err().contains("exact certified base"));
    eprintln!("M-LARGE-ASSET-DELTA-CORRECTNESS {receipt}");
}

#[test]
#[ignore = "measurement harness self-test"]
fn additional_fixed_changes_preserve_full_message_lists_and_stable_personas() {
    let (_directory,mut store) = server_sync::lww_tests::local();
    let generated = tempfile::tempdir().unwrap();
    seed(&mut store,&generated.path().join("fixture"),FixtureScale::small());
    store.commit(&WorkingSetCommit {expected_revision:store.revision().unwrap(),
        unit_mutations:Some(vec![UnitMutation::Set {key:UnitKey::new(&["root","selectedPersona"]).unwrap(),
            value:json!("synthetic-persona-0")}]),..Default::default()}).unwrap();
    for change in [AdditionalChange::Persona,AdditionalChange::MiddleEdit,AdditionalChange::Insertion,
        AdditionalChange::Deletion,AdditionalChange::Burst] {
        let prepared=prepare_additional_change(&store,change,Direction::AtoB,0);
        assert_eq!(prepared.commits.len(),if matches!(change,AdditionalChange::Burst) {16} else {1});
        apply_additional_change(&mut store,&prepared);
        verify_additional_change(&store,&prepared);
    }
}

#[test]
#[ignore = "measurement harness self-test"]
fn additional_native_adapters_use_real_local_transport_in_both_directions() {
    let mut driver=ServerDriver::prepare(FixtureScale::small());
    driver.a.commit(&WorkingSetCommit {expected_revision:driver.a.revision().unwrap(),
        unit_mutations:Some(vec![UnitMutation::Set {key:UnitKey::new(&["root","selectedPersona"]).unwrap(),
            value:json!("synthetic-persona-0")}]),..Default::default()}).unwrap();
    server_sync::lww_tests::publish_cycle(&driver.ca,&mut driver.a,&[]).unwrap().unwrap();
    server_sync::lww_tests::receive_cycle(&driver.cb,&mut driver.b,&[]).unwrap();
    for change in [AdditionalChange::Persona,AdditionalChange::MiddleEdit,AdditionalChange::Insertion,
        AdditionalChange::Deletion,AdditionalChange::Burst] {
        for direction in [Direction::AtoB,Direction::BtoA] {
            let (source,destination,sender,receiver)=match direction {
                Direction::AtoB=>(&mut driver.a,&mut driver.b,&driver.ca,&driver.cb),
                Direction::BtoA=>(&mut driver.b,&mut driver.a,&driver.cb,&driver.ca),
            };
            let prepared=prepare_additional_change(source,change,direction,0);
            apply_additional_change(source,&prepared);
            verify_additional_change(source,&prepared);
            let publication=server_sync::lww_tests::publish_cycle(sender,source,&[]).unwrap().unwrap();
            let completion=server_sync::lww_tests::receive_cycle(receiver,destination,&[]).unwrap();
            assert!(!publication.accepted_keys.is_empty());
            assert!(completion.received_units>0);
            verify_additional_change(destination,&prepared);
        }
    }
}

#[test]
#[ignore = "measurement harness self-test"]
fn additional_native_adapters_use_real_external_cycle_in_both_directions() {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        let mut driver=ExternalDriver::prepare(FixtureScale::small()).await;
        let fixture=&mut driver.fixture;
        fixture.a.commit(&WorkingSetCommit {expected_revision:fixture.a.revision().unwrap(),
            unit_mutations:Some(vec![UnitMutation::Set {key:UnitKey::new(&["root","selectedPersona"]).unwrap(),
                value:json!("synthetic-persona-0")}]),..Default::default()}).unwrap();
        let authority_a=fixture.a.lww_binding_authority().unwrap();
        fixture.sender.publish(&mut fixture.a,authority_a,&[],&Cancellation::default()).await.unwrap();
        let authority_b=fixture.b.lww_binding_authority().unwrap();
        fixture.receiver.receive_and_apply(&mut fixture.b,authority_b,&[],&Cancellation::default()).await.unwrap();
        for change in [AdditionalChange::Persona,AdditionalChange::MiddleEdit,AdditionalChange::Insertion,
            AdditionalChange::Deletion,AdditionalChange::Burst] {
            for direction in [Direction::AtoB,Direction::BtoA] {
                let (source,destination,sender,receiver)=match direction {
                    Direction::AtoB=>(&mut fixture.a,&mut fixture.b,&mut fixture.sender,&mut fixture.receiver),
                    Direction::BtoA=>(&mut fixture.b,&mut fixture.a,&mut fixture.receiver,&mut fixture.sender),
                };
                let prepared=prepare_additional_change(source,change,direction,0);
                apply_additional_change(source,&prepared);
                verify_additional_change(source,&prepared);
                let source_authority=source.lww_binding_authority().unwrap();
                sender.publish(source,source_authority,&[],&Cancellation::default()).await.unwrap();
                let destination_authority=destination.lww_binding_authority().unwrap();
                receiver.receive_and_apply(destination,destination_authority,&[],&Cancellation::default()).await.unwrap();
                verify_additional_change(destination,&prepared);
            }
        }
    });
}

#[test]
#[ignore = "measurement harness self-test"]
fn final_native_local_adapters_prove_owned_identity_metadata_in_fixed_pairs() {
    let _entry=collect_final_local;
    let mut by_scale=Vec::new();
    for scale in frozen_small_scales() {
        let mut driver=ServerDriver::prepare(scale);
        driver.a.commit(&WorkingSetCommit {expected_revision:driver.a.revision().unwrap(),
            unit_mutations:Some(vec![UnitMutation::Set {key:UnitKey::new(&["root","selectedPersona"]).unwrap(),
                value:json!("synthetic-persona-0")}]),..Default::default()}).unwrap();
        server_sync::lww_tests::publish_cycle(&driver.ca,&mut driver.a,&[]).unwrap().unwrap();
        server_sync::lww_tests::receive_cycle(&driver.cb,&mut driver.b,&[]).unwrap();
        let mut samples=Vec::new();
        for &scenario in final_runner::AVAILABLE.iter().filter(|&&scenario|scenario!=measurement::Scenario::SparseBoundary) {
            for direction in [final_runner::Direction::AtoB,final_runner::Direction::BtoA] {
                let (source,destination,sender,receiver)=match direction {
                    final_runner::Direction::AtoB=>(&mut driver.a,&mut driver.b,&driver.ca,&driver.cb),
                    final_runner::Direction::BtoA=>(&mut driver.b,&mut driver.a,&driver.cb,&driver.ca)};
                let sample=final_server_cycle(source,destination,sender,receiver,&driver.asset_hashes,scenario,direction,0,true).unwrap();
                sample.validate().unwrap_or_else(|reason|panic!("{scenario:?}/{direction:?}: {reason}; {}",
                    serde_json::to_string(&sample.observation).unwrap()));
                samples.push(sample);
            }
        }
        by_scale.push(samples);
    }
    let proofs=by_scale[0].iter().zip(&by_scale[1]).map(|(a,b)| {
        assert_eq!(a.selected_keys,b.selected_keys);assert_eq!(a.emitted_keys,b.emitted_keys);
        assert_eq!(a.accepted_winner_keys,b.accepted_winner_keys);assert_eq!(a.affected_keys,b.affected_keys);
        native_observation::assert_frozen_growth(&a.observation,&b.observation).unwrap()
    }).collect::<Vec<_>>();
    assert!(by_scale.iter().flatten().any(|sample|sample.observation.body_owned_work.opens>0),
        "actual verified identity-only owned opens must be exercised");
    eprintln!("M-OWNED-IDENTITY-PAIR-DIAGNOSTIC {}",serde_json::to_string(&json!({
        "status":"CORRECTNESS_ONLY","accepted_measurement":false,"actual_samples":by_scale,"proofs":proofs})).unwrap());
}
#[test]
#[ignore = "measurement harness self-test"]
fn final_native_resume_reopens_same_registration_and_receives_only_missed_units() {
    let mut driver=ServerDriver::prepare(FixtureScale::small());
    assert!(build_final_native_tiers(&mut driver.a,&mut driver.b,|_,_|panic!("a bound existing fixture must fail before construction"))
        .unwrap_err().contains("fresh unbound"));
    let ServerDriver {_server,_directories,mut a,b,ca,cb,receipt,asset_hashes}=driver;
    let (mut b,cb,sample)=final_server_resume_endpoint(&mut a,&ca,b,cb,&asset_hashes,final_runner::Direction::AtoB,0,true).unwrap();
    sample.validate().unwrap();assert!(sample.observation.requests>0);
    let (a,ca,sample)=final_server_resume_endpoint(&mut b,&cb,a,ca,&asset_hashes,final_runner::Direction::BtoA,0,true).unwrap();
    sample.validate().unwrap();assert!(sample.observation.requests>0);
    let _driver=ServerDriver {_server,_directories,a,b,ca,cb,receipt,asset_hashes};
}

#[test]
#[ignore = "measurement harness self-test"]
fn final_native_external_resume_reopens_same_writer_and_actual_bound_engine() {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        let driver=ExternalDriver::prepare(FixtureScale::small()).await;
        let crate::external_storage::lww_tests::CycleFixture {directory_a,directory_b,mut a,b,provider,mut sender,receiver}=driver.fixture;
        let _directories=(directory_a,directory_b);
        let (mut b,mut receiver,first)=final_external_resume_endpoint(&mut a,&mut sender,b,receiver,&provider,
            &driver.asset_hashes,final_runner::Direction::AtoB,0,true).await.unwrap();
        first.validate().unwrap();assert!(first.observation.requests>0);
        let (_a,_sender,second)=final_external_resume_endpoint(&mut b,&mut receiver,a,sender,&provider,
            &driver.asset_hashes,final_runner::Direction::BtoA,0,true).await.unwrap();
        second.validate().unwrap();assert!(second.observation.requests>0);
    });
}

#[test]
#[ignore = "measurement harness self-test"]
fn final_native_bootstrap_retains_real_activation_and_body_receipts_without_source_claim() {
    let driver=ServerDriver::prepare(FixtureScale::small());
    let (_directory,mut destination)=server_sync::lww_tests::local();
    driver._server.prepare_binding_candidate(&destination);
    let writer=destination.lww_clock_state().unwrap().writer_id;
    let record=native_bootstrap::server_bootstrap_native_raw(&mut destination,
        Arc::new(server_sync::client::TestIoCounters::default()),&driver.asset_hashes,
        Some("synthetic-character-0"),final_runner::Direction::AtoB,0).unwrap();
    eprintln!("M-NATIVE-BOOTSTRAP-CORRECTNESS {}",serde_json::to_string(&record).unwrap());
    assert_eq!(record.status,"INVALID");assert!(record.source_end_and_shutdown.is_none());
    assert_eq!(record.writer_before,writer);assert_eq!(record.writer_after.as_ref(),Some(&writer));
    assert!(record.activation_revision.is_some());assert!(record.activation_request.is_some());
    assert!(record.usable_database_ms.is_some());assert!(record.all_bodies_local_ms.is_some());
    assert!(record.native_observation.requests>0);
    assert!(record.native_observation.body_asset_work.staging_written_bytes>0);
    assert!(record.asset_sha_and_publication_ledger.is_some(),"{:?}",record.error);
    assert!(record.error.is_none(),"{:?}",record.error);
    assert_eq!(record.native_observation.shared_head_reads,None);
}

#[test]
#[ignore = "measurement harness self-test"]
fn final_native_sparse_boundary_uses_actual_transport_and_preserves_full_messages() {
    let mut driver=ServerDriver::prepare(FixtureScale::small());
    for direction in [final_runner::Direction::AtoB,final_runner::Direction::BtoA] {
        let (source,destination,sender,receiver)=match direction {
            final_runner::Direction::AtoB=>(&mut driver.a,&mut driver.b,&driver.ca,&driver.cb),
            final_runner::Direction::BtoA=>(&mut driver.b,&mut driver.a,&driver.cb,&driver.ca)};
        let sample=final_server_cycle(source,destination,sender,receiver,&driver.asset_hashes,
            measurement::Scenario::SparseBoundary,direction,0,true).unwrap();
        eprintln!("M-NATIVE-SPARSE-CORRECTNESS {}",serde_json::to_string(&sample).unwrap());
        sample.validate().unwrap();
        assert!(sample.observation.messages_read>0);
        assert!(sample.observation.pages_written>0);
        assert_eq!(sample.selected_keys,sample.emitted_keys);
        assert_eq!(sample.emitted_keys,sample.affected_keys);
    }
}

#[test]
#[ignore = "measurement harness self-test"]
fn actual_compaction_producer_keeps_unobserved_source_and_protection_invalid() {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        let mut fixture=crate::external_storage::lww_tests::CycleFixture::new();
        fixture.a.commit(&WorkingSetCommit {expected_revision:fixture.a.revision().unwrap(),
            unit_mutations:Some(vec![UnitMutation::Set {key:UnitKey::new(&["root","language"]).unwrap(),
                value:json!("ko")}]),..Default::default()}).unwrap();
        fixture.publish_a().await;
        let directory=tempfile::tempdir().unwrap();
        let job_id="00000000-0000-4000-8000-000000000881";
        let writer=fixture.a.lww_clock_state().unwrap().writer_id;
        use crate::external_storage::contract::{Provider,ConnectionConfig,SecretRef,OpenMode};
        let (_,capabilities)=fixture.provider.open_repository(&ConnectionConfig {provider:"fake".into(),
            profile:None,endpoint:"synthetic".into(),account_id:"synthetic".into(),location:Default::default(),
            oauth_profile:None},&SecretRef("synthetic".into()),OpenMode::Existing,&Cancellation::default()).await.unwrap();
        let record=native_maintenance::compact_native_raw(&fixture.sender,&fixture.provider,&mut fixture.a,directory.path(),job_id,&writer,
            &capabilities,None,&[],measurement::Scenario::ConsolidatedSnapshot,0)
            .await.unwrap();
        eprintln!("M-COMPACTION-PRODUCER-CORRECTNESS {}",serde_json::to_string(&record).unwrap());
        assert_eq!(record.status,"INVALID");assert!(record.error.is_none(),"{:?}",record.error);
        assert_eq!(record.snapshot.as_ref().unwrap()["snapshotId"],job_id);
        assert!(record.native_publication_complete_ms.is_some());
        assert!(record.native_observation.requests>0);
        assert!(record.source_observation.is_none());assert!(!record.protection_run_completed);
        assert!(record.native_observation.hash_totals().unwrap().bytes>0);
    });
}

#[test]
#[ignore = "requires coordinator-frozen source libtest identities; tiny endpoint correctness only"]
fn actual_source_process_bootstrap_retains_complete_source_and_native_receipts() {
    let executable=std::path::PathBuf::from(std::env::var("LWW_SOURCE_TEST_EXE").expect("frozen source executable required"));
    let fingerprint=std::env::var("LWW_SOURCE_FINGERPRINT").expect("source fingerprint required");
    let binary=std::env::var("LWW_SOURCE_BINARY_SHA").expect("source binary SHA required");
    let mut source=source_process::SourceProcess::start(&executable,&fingerprint,&binary,"tiny-native-bootstrap").unwrap();
    let (_source_directory,mut producer)=server_sync::lww_tests::local();
    let (_destination_directory,mut destination)=server_sync::lww_tests::local();
    assert_ne!(producer.lww_clock_state().unwrap().writer_id,destination.lww_clock_state().unwrap().writer_id);
    let sender=native_bootstrap::source_client(&mut source,&producer,"synthetic-producer",
        &"82".repeat(32),false).unwrap();
    let config=sender.client.config();
    bind(&mut producer,SyncTarget::Server(config.endpoint.clone()),&config.endpoint,&config.library_id);
    producer.commit(&WorkingSetCommit {expected_revision:producer.revision().unwrap(),
        unit_mutations:Some(vec![UnitMutation::Set {key:UnitKey::new(&["root","language"]).unwrap(),value:json!("ko")}]),
        ..Default::default()}).unwrap();
    let asset=server_sync::lww_tests::put_asset(&mut producer,"assets/synthetic-source-large.png",&vec![37;96*1024]);
    let hash=asset.object_hash.unwrap();
    reset_work(std::slice::from_ref(&hash));
    server_sync::lww_tests::drain_publications(&sender,&mut producer,&[]).unwrap();
    let published=take_work(false);
    let roles=published.body_objects.iter().map(|(hash,object)|(hash.clone(),object.purposes.iter().cloned().collect()))
        .collect::<std::collections::BTreeMap<String,std::collections::BTreeSet<String>>>();
    assert!(roles[&hash].contains("Asset"));
    let candidate=native_bootstrap::source_client(&mut source,&destination,"synthetic-destination",
        &"83".repeat(32),true).unwrap();
    let record=native_bootstrap::server_bootstrap_source_raw(source,&mut destination,
        candidate.client.test_io.unwrap(),&roles,None,final_runner::Direction::AtoB,0).unwrap();
    eprintln!("M-SOURCE-NATIVE-BOOTSTRAP-CORRECTNESS {}",serde_json::to_string(&record).unwrap());
    assert!(record.error.is_none(),"{:?}",record.error);
    assert_eq!(record.status,"INVALID");
    let source:source_receipt::SettledSource=serde_json::from_value(record.source_end_and_shutdown.unwrap()).unwrap();
    source.validate().unwrap();
    assert!(source.end.observation.objects.iter().any(|object|object.hash==hash && object.flow=="source" && object.work.read_bytes>0));
    record.native_observation.validate_costly_native_coverage().unwrap();
    assert!(record.activation_revision.is_some());assert!(record.all_bodies_local_ms.is_some());
}

#[test]
#[ignore = "requires coordinator-frozen source libtest identities; tiny physical overlap correctness only"]
fn actual_source_read_barrier_allows_real_foreground_publication_during_hydration() {
    let executable=std::path::PathBuf::from(std::env::var("LWW_SOURCE_TEST_EXE").expect("frozen source executable required"));
    let fingerprint=std::env::var("LWW_SOURCE_FINGERPRINT").expect("source fingerprint required");
    let binary=std::env::var("LWW_SOURCE_BINARY_SHA").expect("source binary SHA required");
    let mut source=source_process::SourceProcess::start(&executable,&fingerprint,&binary,"tiny-native-physical-overlap").unwrap();
    let (_source_directory,mut producer)=server_sync::lww_tests::local();
    let (_destination_directory,mut destination)=server_sync::lww_tests::local();
    let sender=native_bootstrap::source_client(&mut source,&producer,"synthetic-overlap-producer",
        &"84".repeat(32),false).unwrap();
    let config=sender.client.config();
    bind(&mut producer,SyncTarget::Server(config.endpoint.clone()),&config.endpoint,&config.library_id);
    producer.commit(&WorkingSetCommit {expected_revision:producer.revision().unwrap(),
        unit_mutations:Some(vec![UnitMutation::Set {key:UnitKey::new(&["root","loreBookDepth"]).unwrap(),value:json!(5)}]),
        ..Default::default()}).unwrap();
    let alias=server_sync::lww_tests::put_asset(&mut producer,"assets/synthetic-overlap-large.png",&vec![43;96*1024]);
    let hash=alias.object_hash.unwrap();
    reset_work(std::slice::from_ref(&hash));
    server_sync::lww_tests::drain_publications(&sender,&mut producer,&[]).unwrap();
    let published=take_work(false);
    let roles=published.body_objects.iter().map(|(hash,object)|(hash.clone(),object.purposes.iter().cloned().collect()))
        .collect::<std::collections::BTreeMap<String,std::collections::BTreeSet<String>>>();
    let candidate=native_bootstrap::source_client(&mut source,&destination,"synthetic-overlap-destination",
        &"85".repeat(32),true).unwrap();
    let bound=server_sync::first_binding_cycle(&mut destination,candidate.client.test_io.as_ref().unwrap().clone(),|_|{}).unwrap();
    assert!(bound.activation.revision>=0);
    server_sync::lww_tests::receive_available(&sender,&mut producer,&[]).unwrap();
    let destination_client=LocalServerFixture::reopen_client(&destination,Arc::new(server_sync::client::TestIoCounters::default())).unwrap();
    let cas=crate::asset_repository::PayloadCas::new(destination.repository_root()).unwrap();
    assert!(cas.stat_object(&hash).unwrap().is_none());
    let record=native_bootstrap::server_asset_transfer_overlap_raw(source,&mut destination,&destination_client,
        &mut producer,&sender,&roles,&hash,None,final_runner::Direction::BtoA,0).unwrap();
    eprintln!("M-SOURCE-NATIVE-OVERLAP-CORRECTNESS {}",serde_json::to_string(&record).unwrap());
    assert!(record.errors.is_empty(),"{:?}",record.errors);
    assert_eq!(record.status,"INVALID");
    assert!(record.source_reached.is_some());assert!(record.source_release.is_some());
    assert!(record.foreground_revision.is_some());assert!(record.foreground_publication_ms.is_some());
    assert!(record.native_bodies_settled_ms.is_some());
    assert_eq!(record.native_observation.body_worker_scopes_started,1);
    assert_eq!(record.native_observation.body_worker_scopes_settled,1);
    assert_eq!(record.native_observation.native_worker_hash_receipts.len(),1);
    let source:source_receipt::SettledSource=serde_json::from_value(record.source_end_and_shutdown.unwrap()).unwrap();
    source.validate().unwrap();assert_eq!(source.end.observation.read_barrier.unwrap().status,"released");
    record.native_observation.validate_costly_native_coverage().unwrap();
}

#[test]
#[ignore = "measurement harness self-test"]
fn final_native_sse_constructor_uses_real_projection_and_independent_fresh_writers() {
    let directory=tempfile::tempdir().unwrap();
    let requirement=scale_certificate::ScaleRequirement::Correctness {minimum_database_bytes:4096,minimum_assets:3};
    let configuration=fixture_configuration::NativeFixtureConfiguration {local_sse:Some(fixture_configuration::LocalSseEndpoint {
        address:"127.0.0.1".parse().unwrap(),port:32123})};
    assert!(construct_native_target(Path::new("relative"),requirement,configuration.clone(),|_|Ok(())).is_err());
    let root=directory.path().join("desktop-agent");
    let built=construct_native_target(&root,requirement,configuration.clone(),|_|Ok(())).unwrap();
    assert!(root.join("persistent/persistent.sqlite").is_file());
    assert!(root.join("persistent/device.sqlite").is_file());
    let store=PersistentStore::open(&root).unwrap();
    assert_eq!(store.lww_clock_state().unwrap().writer_id,built.writer_id);
    let generation=super::active_generation(&store.connection).unwrap();
    for index in 0..2 {
        let mut projected=store.read_root(None).unwrap().value.as_object().unwrap().clone();
        projected.insert("botPresetsId".into(),json!(format!("synthetic-preset-{index}")));
        projected.insert("selectedPersona".into(),json!(format!("synthetic-persona-{index}")));
        super::export::derive_identity_root(&store.connection,&generation,&mut projected).unwrap();
        assert_eq!(projected["botPresetsId"],index);assert_eq!(projected["selectedPersona"],index);
        assert_eq!(projected["username"],format!("Synthetic persona {index}"));
        assert_eq!(projected["personaPrompt"],format!("Synthetic persona prompt {index}"));
        assert_eq!(projected["globalNote"],"");
        assert_eq!(projected["userNote"],format!("Synthetic persona note {index}"));
        for key in ["aiModel","subModel"] {assert_eq!(projected[key],"reverse_proxy");}
        assert_eq!(projected["forceReplaceUrl"],"http://127.0.0.1:32123/v1/chat/completions");
        assert_eq!(projected["presetChain"],"");assert_eq!(projected["promptTemplate"],Value::Null);
        assert_eq!(projected["useStreaming"],true);assert_eq!(projected["personaNote"],true);
    }
    assert!(construct_native_target(&root,requirement,configuration.clone(),|_|Ok(())).unwrap_err().contains("empty regular"));
    let android=construct_native_target(&directory.path().join("android-agent"),requirement,
        fixture_configuration::NativeFixtureConfiguration {local_sse:Some(fixture_configuration::LocalSseEndpoint {
            address:"10.0.2.2".parse().unwrap(),port:32123})},|_|Ok(())).unwrap();
    assert_ne!(built.writer_id,android.writer_id);
    assert_eq!(built.fixture.certificate.measured_conversation_sha256,android.fixture.certificate.measured_conversation_sha256);
}

#[test]
#[ignore = "measurement harness self-test"]
fn final_native_external_adapters_return_complete_phase_and_key_evidence() {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        let mut driver=ExternalDriver::prepare(FixtureScale::small()).await;
        driver.fixture.a.commit(&WorkingSetCommit {expected_revision:driver.fixture.a.revision().unwrap(),
            unit_mutations:Some(vec![UnitMutation::Set {key:UnitKey::new(&["root","selectedPersona"]).unwrap(),
                value:json!("synthetic-persona-0")}]),..Default::default()}).unwrap();
        let authority=driver.fixture.a.lww_binding_authority().unwrap();
        driver.fixture.sender.publish(&mut driver.fixture.a,authority,&[],&Cancellation::default()).await.unwrap();
        let authority=driver.fixture.b.lww_binding_authority().unwrap();
        driver.fixture.receiver.receive_and_apply(&mut driver.fixture.b,authority,&[],&Cancellation::default()).await.unwrap();
        for &scenario in final_runner::AVAILABLE {
            for direction in [final_runner::Direction::AtoB,final_runner::Direction::BtoA] {
                let mut adapter=FinalExternalAdapter {fixture:&mut driver.fixture,asset_hashes:&driver.asset_hashes};
                let sample=final_runner::AsyncCycleDriver::cycle(&mut adapter,scenario,direction,0,true).await.unwrap();
                sample.validate().unwrap_or_else(|reason|panic!("{scenario:?}/{direction:?}: {reason}; {}",
                    serde_json::to_string(&sample.observation).unwrap()));
                assert!(sample.observation.hash_totals().unwrap().calls>0);
                assert!(sample.observation.requests>0);
            }
        }
    });
}

#[test]
#[ignore = "measurement harness self-test"]
fn frozen_fixture_stages_real_native_rows_and_unique_message_ids() {
    let (directory, mut store) = server_sync::lww_tests::local();
    let generated = tempfile::tempdir().unwrap();
    let receipt = seed(&mut store, &generated.path().join("fixture"), FixtureScale::small());
    assert_eq!(receipt.scale.assets, 64);
    assert_eq!(receipt.scale.long_conversation_messages, 4096);
    let (commit, expected) = prepare_edit(&store, SmallChange::Append, Direction::AtoB, 0);
    store.commit(&commit).unwrap();
    verify(&store, SmallChange::Append, &expected);
    store.checkpoint(super::CheckpointMode::Truncate).unwrap();
    let mut reopened = PersistentStore::open(directory.path()).unwrap();
    verify(&reopened, SmallChange::Append, &expected);
    assert!(reopened.storage_stats().unwrap().database_bytes > 0);
    let mut prior = expected[0].clone();
    prior["data"] = json!("synthetic corrupted prefix");
    reopened.commit(&WorkingSetCommit {
        expected_revision:reopened.revision().unwrap(),
        conversations:Some(vec![ConversationMutation::ReplaceRange {
            character_id:"synthetic-character-0".into(), conversation_id:"synthetic-conversation-0".into(),
            start:0, delete_count:1, messages:vec![prior], conversation:None, configured_index:None }]),
        messages_changed:Some(vec![MessageLocator {
            character_id:"synthetic-character-0".into(), conversation_id:"synthetic-conversation-0".into(), start:Some(0) }]),
        ..Default::default()
    }).unwrap();
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(||
        verify(&reopened, SmallChange::Append, &expected))).is_err(),
        "same last message and unique IDs must not hide a changed earlier prefix");
}

#[test]
#[ignore = "measurement harness self-test"]
fn external_fixture_bootstrap_correctness() {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        let mut driver = ExternalDriver::prepare(FixtureScale::small()).await;
        assert_eq!(driver.fixture.a.read_conversation("synthetic-character-0", "synthetic-conversation-0", None)
            .unwrap().unwrap().value, driver.fixture.b.read_conversation("synthetic-character-0",
                "synthetic-conversation-0", None).unwrap().unwrap().value);
        assert_eq!(driver.fixture.provider.read_attempts("head"), 0);
        let reads = driver.fixture.provider.read_count();
        let bytes = driver.fixture.provider.transferred_body_bytes();
        let authority = driver.fixture.a.lww_binding_authority().unwrap();
        assert_eq!(driver.fixture.sender.receive_and_apply(&mut driver.fixture.a, authority, &[],
            &Cancellation::default()).await.unwrap(), 0);
        assert_eq!(driver.fixture.provider.read_count(), reads);
        assert_eq!(driver.fixture.provider.transferred_body_bytes(), bytes);
        verify_settled(&driver.fixture.a, &driver.fixture.sender);
        verify_settled(&driver.fixture.b, &driver.fixture.receiver);
    });
}

#[test]
#[ignore = "measurement harness self-test"]
fn local_server_fixture_bootstrap_correctness() {
    let driver = ServerDriver::prepare(FixtureScale::small());
    verify_bootstrap(&driver.a, &driver.b);
}

#[test]
#[ignore = "measurement harness self-test"]
fn local_routine_observation_regression() {
    let mut driver = ServerDriver::prepare(FixtureScale::small());
    for change in [SmallChange::Setting, SmallChange::Preset, SmallChange::Append] {
        for direction in [Direction::AtoB, Direction::BtoA] {
            let sample = driver.run(direction, change, 0);
            sample.observation.validate_routine_invariants().unwrap();
            assert_eq!(native_observation::assert_frozen_growth(&sample.observation,&sample.observation)
                .unwrap().aggregate_control_growth,0);
            let evidence=sample.observation.receive_intents.as_ref().unwrap();
            assert!(evidence.complete);
            assert_eq!(evidence.observed_calls,evidence.captured_calls);
            assert_eq!(evidence.observed_bytes,evidence.captured_bytes);
            assert_eq!(evidence.inputs.iter().map(|input|input.input_bytes).sum::<u64>(),evidence.observed_bytes);
            let evidence=sample.observation.commit_intents.as_ref().unwrap();
            assert!(evidence.complete);
            assert_eq!(evidence.observed_calls,evidence.captured_calls);
            assert_eq!(evidence.observed_bytes,evidence.captured_bytes);
            assert_eq!(evidence.inputs.iter().map(|input|input.input_bytes).sum::<u64>(),evidence.observed_bytes);
        }
    }
}

#[test]
#[ignore = "measurement harness self-test"]
fn actual_body_observer_binding_counts_reads_and_preserves_catalog_only_scope() {
    let directory = tempfile::tempdir().unwrap();
    let cas = crate::asset_repository::PayloadCas::new(directory.path()).unwrap();
    let object = cas.prepare_bytes(b"synthetic-body-data").unwrap();
    let untouched = (0..1024).map(|index| format!("{index:064x}")).collect::<Vec<_>>();
    reset_work(&untouched);
    assert_eq!(cas.stat_object(&object.content_hash).unwrap(), Some(19));
    let mut catalog = take_work(false);
    catalog.shared_head_reads = Some(0);
    assert_eq!(catalog.asset_body_opens, Some(0));
    assert_eq!(catalog.asset_body_bytes_read, Some(0));
    assert_eq!(catalog.body_stat_requests, 1);
    assert!(catalog.body_objects.is_empty());
    catalog.validate_routine_invariants().unwrap();

    reset_work(std::slice::from_ref(&object.content_hash));
    crate::asset_repository::body_io::register_object_purpose(&object.content_hash,
        crate::asset_repository::body_io::BodyPurpose::Control);
    assert_eq!(cas.read_object(&object.content_hash).unwrap().unwrap(), b"synthetic-body-data");
    let mut body = take_work(false);
    body.shared_head_reads = Some(0);
    assert_eq!(body.asset_body_opens, Some(1));
    assert_eq!(body.asset_body_bytes_read, Some(19));
    assert_eq!(body.body_domains["managed"].read_operations, 1);
    assert_eq!(body.body_asset_work.read_bytes, 19);
    assert_eq!(body.body_control_work, BodyDomain::default());
    assert_eq!(body.body_scope_complete, Some(true));
    assert_eq!(body.body_objects.len(), 1);
    assert_eq!(body.body_objects[&object.content_hash].purposes, vec!["Control", "Asset"]);
    assert!(body.validate_routine_invariants().is_err());
}

#[test]
#[ignore = "measurement harness self-test"]
fn actual_body_observer_distinguishes_control_and_unknown_reads() {
    use crate::asset_repository::body_io::{register_object_purpose, BodyPurpose};
    let directory = tempfile::tempdir().unwrap();
    let cas = crate::asset_repository::PayloadCas::new(directory.path()).unwrap();
    let object = cas.prepare_bytes(b"synthetic-control-data").unwrap();
    let untouched = (0..1024).map(|index| format!("{index:064x}")).collect::<Vec<_>>();
    reset_work(&untouched);
    register_object_purpose(&object.content_hash, BodyPurpose::Control);
    assert_eq!(cas.read_object(&object.content_hash).unwrap().unwrap(), b"synthetic-control-data");
    let mut control = take_work(false);
    control.shared_head_reads = Some(0);
    assert_eq!(control.body_control_work.open_attempts, 1);
    assert_eq!(control.body_control_work.read_bytes, 22);
    assert_eq!(control.body_objects[&object.content_hash].purposes, vec!["Control"]);
    assert_eq!(control.body_objects.len(), 1);
    assert_eq!(control.asset_body_opens, Some(0));
    control.validate_routine_invariants().unwrap();

    reset_work(&[]);
    assert_eq!(cas.read_object(&object.content_hash).unwrap().unwrap(), b"synthetic-control-data");
    let mut unknown = take_work(false);
    unknown.shared_head_reads = Some(0);
    assert_eq!(unknown.body_unknown_work.open_attempts, 1);
    assert_eq!(unknown.body_unknown_work.read_bytes, 22);
    assert_eq!(unknown.body_scope_complete, Some(false));
    assert_eq!(unknown.body_objects.len(), 1);
    assert!(unknown.body_objects[&object.content_hash].purposes.is_empty());
    assert!(unknown.validate_routine_invariants().is_err());
}

#[test]
#[ignore = "measurement harness self-test"]
fn actual_body_observer_retains_failed_asset_and_owned_only_work() {
    use crate::asset_repository::body_io::{register_object_purpose, BodyPurpose};
    use sha2::{Digest, Sha256};
    let directory = tempfile::tempdir().unwrap();
    let cas = crate::asset_repository::PayloadCas::new(directory.path()).unwrap();
    let missing = "ab".repeat(32);
    reset_work(std::slice::from_ref(&missing));
    {
        let _object_scope = crate::asset_repository::body_io::object_scope(&missing);
        let opened = std::fs::File::open(directory.path().join("missing-body"));
        assert!(opened.is_err());
        crate::asset_repository::body_io::open_result("managed", &opened);
    }
    let mut failed = take_work(false);
    failed.shared_head_reads = Some(0);
    assert_eq!(failed.body_objects.len(), 1);
    assert_eq!(failed.body_objects[&missing].work.failed_opens, 1);
    assert_eq!(failed.body_asset_work.open_attempts, 1);
    assert!(failed.validate_routine_invariants().is_err());

    let bytes = b"synthetic staged control";
    let hash = hex::encode(Sha256::digest(bytes));
    reset_work(&[]);
    register_object_purpose(&hash, BodyPurpose::Control);
    let staged = cas.stage_reader_expected(&mut bytes.as_slice(), &hash, bytes.len() as u64).unwrap();
    drop(staged);
    let owned = take_work(false);
    assert_eq!(owned.body_scope_complete, Some(true));
    assert_eq!(owned.body_objects.len(), 1);
    assert_eq!(owned.body_objects[&hash].work, BodyDomain::default());
    assert_eq!(owned.body_objects[&hash].owned_work.staging_written_bytes, bytes.len() as u64);
    assert_eq!(owned.body_objects[&hash].owned_work.body_sha["cas_stage"].bytes, bytes.len() as u64);
    owned.validate_costly_native_coverage().unwrap();
}

#[test]
#[ignore = "measurement harness self-test"]
fn actual_body_observer_binding_rejects_escaped_file_and_path() {
    let directory = tempfile::tempdir().unwrap();
    let cas = crate::asset_repository::PayloadCas::new(directory.path()).unwrap();
    let object = cas.prepare_bytes(b"synthetic-body-data").unwrap();
    reset_work(std::slice::from_ref(&object.content_hash));
    let file = cas.open_object(&object.content_hash).unwrap().unwrap();
    let mut escaped = take_work(false);
    escaped.shared_head_reads = Some(0);
    assert_eq!(escaped.body_domains["managed"].escaped_handles, 1);
    assert_eq!(escaped.body_scope_complete, Some(false));
    assert!(escaped.validate_routine_invariants().is_err());
    drop(file);
    reset_work(std::slice::from_ref(&object.content_hash));
    assert!(cas.object_path(&object.content_hash).unwrap().is_some());
    let mut path = take_work(false);
    path.shared_head_reads = Some(0);
    assert_eq!(path.asset_body_opens, Some(0));
    assert_eq!(path.body_domains["managed"].escaped_paths, 1);
    assert_eq!(path.body_scope_complete, Some(false));
    assert!(path.validate_routine_invariants().is_err());
}

#[cfg(target_os = "windows")]
#[test]
#[ignore = "requires root's verified Wave2 checkpoint and complete body observers"]
fn windows_fake_provider_smoke() {
    measurement_gate();
    let output = std::env::var_os("RISUNEST_LWW_NATIVE_OUTPUT").expect("separate synthetic output directory");
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        let mut by_scale = Vec::new();
        for scale in frozen_small_scales() {
            let mut driver = ExternalDriver::prepare(scale).await;
            let mut samples = Vec::new();
            for iteration in 0..4 {
                for change in [SmallChange::Setting, SmallChange::Preset, SmallChange::Append] {
                    for direction in [Direction::AtoB, Direction::BtoA] {
                        let sample = driver.run(direction, change, iteration).await;
                        if iteration > 0 { samples.push(sample); }
                    }
                }
            }
            by_scale.push(samples);
        }
        let attributions=validate_smoke_pairs(Path::new(&output),&by_scale).unwrap();
        write_samples(Path::new(&output), &by_scale.into_iter().flatten().collect::<Vec<_>>(),&attributions);
    });
}

#[cfg(target_os = "windows")]
#[test]
#[ignore = "requires root's verified Wave2 checkpoint and complete body observers"]
fn windows_local_server_smoke() {
    measurement_gate();
    let output = std::env::var_os("RISUNEST_LWW_NATIVE_OUTPUT").expect("separate synthetic output directory");
    let mut by_scale = Vec::new();
    for scale in frozen_small_scales() {
        let mut driver = ServerDriver::prepare(scale);
        let mut samples = Vec::new();
        for iteration in 0..4 {
            for change in [SmallChange::Setting, SmallChange::Preset, SmallChange::Append] {
                for direction in [Direction::AtoB, Direction::BtoA] {
                    let sample = driver.run(direction, change, iteration);
                    if iteration > 0 { samples.push(sample); }
                }
            }
        }
        by_scale.push(samples);
    }
    let attributions=validate_smoke_pairs(Path::new(&output),&by_scale).unwrap();
    write_samples(Path::new(&output), &by_scale.into_iter().flatten().collect::<Vec<_>>(),&attributions);
}

#[test]
#[ignore = "measurement harness self-test"]
fn failed_pair_persists_all_positive_domains_without_accepted_output() {
    fn sample(bytes:u64) -> NativeSample {
        let mut observation=NativeObservation::default();
        observation.hashes.insert("native/native_receive_intent".into(),native_observation::HashDomain {calls:1,bytes});
        observation.hashes.insert("other/domain".into(),native_observation::HashDomain {calls:1,bytes});
        NativeSample {transport:"diagnostic-validator",direction:Direction::AtoB,change:SmallChange::Setting,
            iteration:1,fixture_scale:FixtureScale::small(),generated_json_bytes:0,source_database_bytes_before_change:0,
            selected_keys:Some(Vec::new()),emitted_keys:Some(Vec::new()),accepted_keys:Some(Vec::new()),
            affected_keys:Some(Vec::new()),external_key_scope:None,observation,
            durable_save_ms:0.0,publication_complete_ms:0.0,receiver_durable_complete_ms:0.0}
    }
    let directory=tempfile::tempdir().unwrap();
    let output=directory.path().join("invalid");
    let matrix=vec![vec![sample(10)],vec![sample(11)]];
    assert!(validate_smoke_pairs(&output,&matrix).is_err());
    assert!(!output.join("native-smoke.json").exists());
    let artifact:Value=serde_json::from_reader(File::open(output.join("INVALID-native-diagnostics.json")).unwrap()).unwrap();
    assert_eq!(artifact["status"],"INVALID");
    assert_eq!(artifact["accepted"],false);
    assert_eq!(artifact["comparisons"][0]["all_positive_hash_domain_growth"].as_array().unwrap().len(),2);
    assert_eq!(artifact["all_post_take_samples"].as_array().unwrap().len(),2);
}

#[cfg(target_os = "windows")]
#[test]
#[ignore = "explicit diagnostic only, always writes INVALID output"]
fn local_receive_intent_growth_diagnostic() {
    let output=std::env::var_os("RISUNEST_LWW_NATIVE_OUTPUT").expect("fresh separate diagnostic directory");
    let mut by_scale=Vec::new();
    for scale in frozen_small_scales() {
        let mut driver=ServerDriver::prepare(scale);
        let mut samples=Vec::new();
        for iteration in 0..4 {
            for change in [SmallChange::Setting,SmallChange::Preset,SmallChange::Append] {
                for direction in [Direction::AtoB,Direction::BtoA] {
                    samples.push(driver.run(direction,change,iteration));
                }
            }
        }
        by_scale.push(samples);
    }
    write_invalid_diagnostics(Path::new(&output),&by_scale,
        "diagnostic only: all executed warmup and recorded operations retained; no acceptance authorized",None);
}

#[cfg(target_os = "windows")]
#[test]
#[ignore = "explicit FakeProvider diagnostic only, always writes INVALID output"]
fn fake_provider_intent_growth_diagnostic() {
    let output=std::env::var_os("RISUNEST_LWW_NATIVE_OUTPUT").expect("fresh separate diagnostic directory");
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
        let mut by_scale=Vec::new();
        for scale in frozen_small_scales() {
            let mut driver=ExternalDriver::prepare(scale).await;
            let mut samples=Vec::new();
            for iteration in 0..4 {
                for change in [SmallChange::Setting,SmallChange::Preset,SmallChange::Append] {
                    for direction in [Direction::AtoB,Direction::BtoA] {
                        samples.push(driver.run(direction,change,iteration).await);
                    }
                }
            }
            by_scale.push(samples);
        }
        write_invalid_diagnostics(Path::new(&output),&by_scale,
            "FakeProvider diagnostic only: all executed warmup and recorded operations retained; no acceptance authorized",None);
    });
}
