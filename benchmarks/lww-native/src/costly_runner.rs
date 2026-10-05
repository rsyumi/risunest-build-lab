use super::{final_runner::Direction, measurement::{percentiles,Percentiles,Scenario},
    native_observation::{BodyDomain,NativeObservation},source_receipt::SettledSource};
use serde::Serialize;
use std::collections::{BTreeMap,BTreeSet};

pub const WARMUPS:u32=1;
pub const REPETITIONS:u32=5;
pub const SCENARIOS:&[Scenario]=&[Scenario::FullBootstrap,Scenario::RestoreAllPresent,Scenario::RestoreMissing,
    Scenario::ConsolidatedSnapshot,Scenario::IncomparableSnapshots,Scenario::DuringCompaction,
    Scenario::DuringBackup,Scenario::DuringAssetTransfer];

#[derive(Clone,Debug,Serialize)]
pub struct AssetInventory {
    pub producer_identity:String,
    pub required:BTreeMap<String,u64>,
    pub already_present:BTreeSet<String>,
    pub missing:BTreeSet<String>,
}
impl AssetInventory {
    pub fn validate(&self)->Result<(),String> {
        if !hash(&self.producer_identity) || self.required.is_empty()
            || self.required.keys().any(|key|!hash(key))
            || !self.already_present.is_disjoint(&self.missing)
            || self.already_present.union(&self.missing).cloned().collect::<BTreeSet<_>>()
                !=self.required.keys().cloned().collect() {
            return Err("verified producer asset inventory is incomplete or overlapping".into());
        }
        Ok(())
    }
}
fn hash(value:&str)->bool {value.len()==64 && value.bytes().all(|b|b.is_ascii_digit() || (b'a'..=b'f').contains(&b))}

#[derive(Clone,Debug,Serialize)]
pub struct ActivationReceipt {
    pub request_id:String,
    pub staging_id:String,
    pub receive_id:String,
    pub revision:i64,
    pub authority:String,
    pub replay_revision:i64,
    pub revision_before_replay:i64,
    pub revision_after_replay:i64,
}
impl ActivationReceipt {
    fn validate(&self)->Result<(),String> {
        let authority=self.authority.parse::<u64>().map_err(|_|"activation authority is not canonical")?;
        if self.request_id.is_empty() || self.staging_id.is_empty() || self.receive_id.is_empty()
            || self.revision<0 || authority.to_string()!=self.authority || self.replay_revision!=self.revision
            || self.revision_before_replay<self.revision || self.revision_after_replay!=self.revision_before_replay {
            return Err("actual durable activation and exact replay receipt disagree".into());
        }
        Ok(())
    }
}

#[derive(Clone,Debug,Serialize)]
pub struct BodyCompletionReceipt {
    pub activation_revision:i64,
    pub settled_asset_sizes:BTreeMap<String,u64>,
    pub worker_completion:String,
    pub protections_released:bool,
}

#[derive(Clone,Debug,Default,Serialize)]
pub struct CostlyTimings {
    pub native_activation_ms:Option<f64>,
    pub native_bodies_settled_ms:Option<f64>,
    pub foreground_durable_ms:Option<f64>,
    pub background_settled_ms:Option<f64>,
    pub renderer_adopted_ms:Option<f64>,
}

#[derive(Clone,Debug,Serialize)]
pub struct OverlapReceipt {
    pub barrier_kind:String,
    pub barrier_identity:String,
    pub reached:bool,
    pub foreground_request_id:String,
    pub foreground_revision:i64,
    pub foreground_completed_while_held:bool,
    pub released:bool,
    pub background_completion:String,
}

#[derive(Clone,Debug,Serialize)]
pub struct CostlySample {
    pub scenario:Scenario,
    pub direction:Direction,
    pub iteration:u32,
    pub warmup:bool,
    pub source_kind:String,
    pub destination_writer_before:Option<String>,
    pub destination_writer_after:Option<String>,
    pub inventory:Option<AssetInventory>,
    pub activation:Option<ActivationReceipt>,
    pub bodies:Option<BodyCompletionReceipt>,
    pub overlap:Option<OverlapReceipt>,
    pub native:Option<NativeObservation>,
    pub source:Option<SettledSource>,
    pub timings:CostlyTimings,
    pub owner_raw_receipts:BTreeMap<String,serde_json::Value>,
    pub errors:Vec<String>,
}

fn active(work:&BodyDomain)->bool {work!=&BodyDomain::default()}
impl CostlySample {
    pub fn validate_server_body_cycle(&self)->Result<(),String> {
        if !matches!(self.scenario,Scenario::FullBootstrap|Scenario::RestoreAllPresent|Scenario::RestoreMissing|Scenario::DuringAssetTransfer)
            || self.iteration>=WARMUPS+REPETITIONS
            || self.warmup!=(self.iteration<WARMUPS) || !self.errors.is_empty()
            || self.source_kind!="native-server-source-process" {
            return Err("costly slot, source adapter or raw operation failed".into());
        }
        let before=self.destination_writer_before.as_ref().ok_or("destination writer unobserved")?;
        if before.is_empty() || self.destination_writer_after.as_ref()!=Some(before) {
            return Err("fresh destination writer continuity unobserved".into());
        }
        let inventory=self.inventory.as_ref().ok_or("actual producer inventory unobserved")?;
        inventory.validate()?;
        let native=self.native.as_ref().ok_or("native complete scope unobserved")?;
        native.validate_costly_native_coverage()?;
        let source=self.source.as_ref().ok_or("source End and final Shutdown unobserved")?;
        source.validate()?;
        let snapshot=&source.end.observation;
        let mut source_assets=BTreeSet::new();
        for object in &snapshot.objects {
            if object.purposes.iter().any(|purpose|purpose=="Asset") {
                if !inventory.required.contains_key(&object.hash) {return Err("source touched an undeclared Asset".into());}
                if !object.work.asset_zero() {
                    if inventory.already_present.contains(&object.hash) {return Err("source touched an already-present Asset".into());}
                    let size=inventory.required[&object.hash];
                    if object.work.opens>0 && object.work.read_calls>0 && (size==0 || object.work.read_bytes>=size) {
                        source_assets.insert(object.hash.clone());
                    }
                }
            }
        }
        let mut destination_assets=BTreeSet::new();
        for (hash,object) in &native.body_objects {
            if object.purposes.iter().any(|purpose|purpose=="Asset") {
                if !inventory.required.contains_key(hash) {return Err("destination touched an undeclared Asset".into());}
                if active(&object.work) || active(&object.owned_work) {
                    if inventory.already_present.contains(hash) {return Err("destination copied, read, hashed or relinked an already-present Asset".into());}
                    if object.owned_work.staging_written_bytes>=inventory.required[hash]
                        && object.owned_work.body_sha.get("cas_stage").is_some_and(|work|work.calls>0)
                        && object.work.publications>0 {
                        destination_assets.insert(hash.clone());
                    }
                }
            }
        }
        if self.scenario==Scenario::RestoreAllPresent && !inventory.missing.is_empty() {
            return Err("all-present restore includes missing assets".into());
        }
        if source_assets!=inventory.missing || destination_assets!=inventory.missing {
            return Err("actual source reads and destination installs do not cover exactly the missing subset".into());
        }
        let activation=self.activation.as_ref().ok_or("durable activation receipt unobserved")?;
        activation.validate()?;
        let bodies=self.bodies.as_ref().ok_or("actual all-body completion receipt unobserved")?;
        if bodies.activation_revision!=activation.revision || bodies.settled_asset_sizes!=inventory.required
            || bodies.worker_completion.is_empty() || !bodies.protections_released {
            return Err("body completion is not tied to the actual activation and required inventory".into());
        }
        let activation_ms=self.timings.native_activation_ms.ok_or("native activation timing unobserved")?;
        let bodies_ms=self.timings.native_bodies_settled_ms.ok_or("native body completion timing unobserved")?;
        if !activation_ms.is_finite() || !bodies_ms.is_finite() || activation_ms<0.0 || bodies_ms<activation_ms {
            return Err("native activation/body timing order invalid".into());
        }
        if self.timings.renderer_adopted_ms.is_some() {return Err("native-only adapter cannot produce renderer adoption timing".into());}
        if self.scenario==Scenario::DuringAssetTransfer {
            let overlap=self.overlap.as_ref().ok_or("actual physical read overlap unobserved")?;
            let barrier=snapshot.read_barrier.as_ref().ok_or("physical source read barrier receipt absent")?;
            if overlap.barrier_kind!="source-physical-read" || overlap.barrier_identity!=barrier.barrier_id
                || !overlap.reached || !overlap.foreground_completed_while_held || !overlap.released
                || overlap.foreground_request_id.is_empty() || overlap.foreground_revision<activation.revision
                || overlap.background_completion!=bodies.worker_completion {
                return Err("foreground edit did not finish under the actual physical read barrier".into());
            }
            let foreground=self.timings.foreground_durable_ms.ok_or("foreground durability timing unobserved")?;
            let background=self.timings.background_settled_ms.ok_or("background completion timing unobserved")?;
            if !foreground.is_finite() || foreground<0.0 || !background.is_finite() || background<foreground {
                return Err("foreground/background timing order invalid".into());
            }
        }
        Ok(())
    }
}

#[derive(Debug,Serialize)]
pub struct CostlySlot {pub scenario:Scenario,pub direction:Direction,pub iteration:u32,pub warmup:bool}
pub fn slots()->Vec<CostlySlot> {
    SCENARIOS.iter().flat_map(|&scenario|[Direction::AtoB,Direction::BtoA].into_iter().flat_map(move |direction|
        (0..WARMUPS+REPETITIONS).map(move |iteration|CostlySlot {scenario,direction,iteration,warmup:iteration<WARMUPS}))).collect()
}
#[derive(Debug,Serialize)]
pub struct CostlySummary {
    pub scenario:Scenario,pub direction:Direction,pub repetitions:u32,pub tail_resolution:&'static str,
    pub native_activation_ms:Percentiles,pub native_bodies_settled_ms:Percentiles,
}
pub fn summarize_server(samples:&[CostlySample],scenario:Scenario)->Result<Vec<CostlySummary>,String> {
    if samples.len()!=2*(WARMUPS+REPETITIONS) as usize {return Err("incomplete costly matrix".into());}
    let mut summaries=Vec::new();let mut index=0;
    for direction in [Direction::AtoB,Direction::BtoA] {
        for iteration in 0..WARMUPS+REPETITIONS {
            let sample=&samples[index];index+=1;
            if (sample.scenario,sample.direction,sample.iteration,sample.warmup)!=(scenario,direction,iteration,iteration<WARMUPS) {
                return Err("costly matrix reordered, omitted or duplicated a slot".into());
            }
            sample.validate_server_body_cycle()?;
        }
        let recorded=samples.iter().filter(|sample|sample.direction==direction && !sample.warmup).collect::<Vec<_>>();
        summaries.push(CostlySummary {scenario,direction,repetitions:REPETITIONS,
            tail_resolution:"N=5, nearest-rank p95/p99 equal the recorded maximum; population tail is unresolved",
            native_activation_ms:percentiles(recorded.iter().map(|sample|sample.timings.native_activation_ms.unwrap()))?,
            native_bodies_settled_ms:percentiles(recorded.iter().map(|sample|sample.timings.native_bodies_settled_ms.unwrap()))?});
    }
    Ok(summaries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::source_receipt::{Snapshot,Work,PendingBodyWork,EndReceipt,ShutdownReceipt};
    fn all_present()->CostlySample {
        let inventory=AssetInventory {producer_identity:"a".repeat(64),required:BTreeMap::from([("b".repeat(64),20)]),
            already_present:BTreeSet::from(["b".repeat(64)]),missing:BTreeSet::new()};
        let snapshot=Snapshot {scope_id:"scope".into(),generation:1,phase:"restore".into(),role_count:1,complete:true,
            objects:vec![],placements:BTreeMap::new(),purposes:BTreeMap::new(),flows:BTreeMap::new(),hash_domains:BTreeMap::new(),
            total:Work::default(),unknown_work:Work::default(),violations:BTreeMap::new(),pending_workers:0,pending_requests:0,
            pending_body_work:PendingBodyWork::default(),read_barrier:None};
        let native=NativeObservation {body_scope_complete:Some(true),native_caller_hashes:Some(BTreeMap::new()),..Default::default()};
        CostlySample {scenario:Scenario::RestoreAllPresent,direction:Direction::AtoB,iteration:0,warmup:true,
            source_kind:"native-server-source-process".into(),destination_writer_before:Some("independent-writer".into()),
            destination_writer_after:Some("independent-writer".into()),inventory:Some(inventory.clone()),
            activation:Some(ActivationReceipt {request_id:"request".into(),staging_id:"stage".into(),receive_id:"receive".into(),
                revision:3,authority:"1".into(),replay_revision:3,revision_before_replay:4,revision_after_replay:4}),
            bodies:Some(BodyCompletionReceipt {activation_revision:3,settled_asset_sizes:inventory.required,worker_completion:"settled".into(),protections_released:true}),
            overlap:None,native:Some(native),source:Some(SettledSource {end:EndReceipt {scope_id:"scope".into(),observation:snapshot.clone()},
                shutdown:ShutdownReceipt {complete:true,root_removed:true,observation:snapshot,child_exit:0}}),
            timings:CostlyTimings {native_activation_ms:Some(1.0),native_bodies_settled_ms:Some(2.0),..Default::default()},
            owner_raw_receipts:BTreeMap::new(),errors:vec![]}
    }
    #[test]
    #[ignore = "measurement harness self-test; benchmarks/lww-native/run.ps1 runs it"]
    fn all_present_requires_real_complete_zero_source_destination_and_body_receipts() {
        let valid=all_present();valid.validate_server_body_cycle().unwrap();
        let mut sample=valid.clone();sample.source=None;assert!(sample.validate_server_body_cycle().is_err());
        sample=valid.clone();sample.bodies=None;assert!(sample.validate_server_body_cycle().is_err());
        sample=valid.clone();sample.activation.as_mut().unwrap().replay_revision=4;assert!(sample.validate_server_body_cycle().is_err());
        sample=valid.clone();sample.bodies.as_mut().unwrap().protections_released=false;assert!(sample.validate_server_body_cycle().is_err());
        sample=valid.clone();sample.native.as_mut().unwrap().body_asset_work.staging_written_bytes=20;assert!(sample.validate_server_body_cycle().is_err());
        sample=valid.clone();sample.source.as_mut().unwrap().shutdown.child_exit=1;assert!(sample.validate_server_body_cycle().is_err());
        sample=valid.clone();sample.timings.renderer_adopted_ms=Some(1.0);assert!(sample.validate_server_body_cycle().is_err());
        sample=valid;sample.destination_writer_after=Some("copied-writer".into());assert!(sample.validate_server_body_cycle().is_err());
    }
    #[test]
    #[ignore = "measurement harness self-test; benchmarks/lww-native/run.ps1 runs it"]
    fn missing_inventory_is_exact_and_missing_work_cannot_be_replaced_by_activation() {
        let mut sample=all_present();sample.scenario=Scenario::RestoreMissing;
        let inventory=sample.inventory.as_mut().unwrap();inventory.already_present.clear();inventory.missing.insert("b".repeat(64));
        inventory.validate().unwrap();assert!(sample.validate_server_body_cycle().is_err());
        let inventory=sample.inventory.as_mut().unwrap();
        inventory.already_present.insert("b".repeat(64));assert!(inventory.validate().is_err());
    }
    #[test]
    #[ignore = "measurement harness self-test; benchmarks/lww-native/run.ps1 runs it"]
    fn costly_matrix_retains_warmup_and_exact_five_recorded_slots_in_both_directions() {
        let slots=slots();assert_eq!(slots.len(),8*2*6);
        assert_eq!(slots.iter().filter(|slot|slot.warmup).count(),16);
        assert_eq!(slots.iter().filter(|slot|!slot.warmup).count(),80);
        let mut samples=Vec::new();for direction in [Direction::AtoB,Direction::BtoA] {for iteration in 0..6 {
            let mut sample=all_present();sample.direction=direction;sample.iteration=iteration;sample.warmup=iteration==0;samples.push(sample);
        }}
        let summaries=summarize_server(&samples,Scenario::RestoreAllPresent).unwrap();assert_eq!(summaries.len(),2);
        assert_eq!(summaries[0].repetitions,5);samples.swap(0,1);assert!(summarize_server(&samples,Scenario::RestoreAllPresent).is_err());
    }
}
