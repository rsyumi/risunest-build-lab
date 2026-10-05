use super::{measurement::{percentiles, Percentiles, Scenario, ALL_SCENARIOS},
    native_observation::{assert_frozen_growth, FrozenGrowthProof, NativeObservation},
    scale_certificate::{validate_above_target, ScaleCertificate}};
use serde::{Serialize,Deserialize};

pub const WARMUPS:u32 = 5;
pub const REPETITIONS:u32 = 30;
pub const AVAILABLE:&[Scenario] = &[Scenario::SettingEdit,Scenario::PresetSwitch,Scenario::PersonaSwitch,
    Scenario::Append,Scenario::MiddleEdit,Scenario::Insertion,Scenario::Deletion,Scenario::OversizedMessage,Scenario::Burst,
    Scenario::SparseBoundary];

#[derive(Clone,Copy,Debug,Serialize,Deserialize,PartialEq,Eq)]
pub enum Direction { AtoB, BtoA }

#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct NativeTimings {
    pub durable_save_ms:f64,
    pub publication_complete_ms:f64,
    pub receiver_durable_ack_ms:f64,
}

#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct RoutineSample {
    pub scenario:Scenario,
    pub direction:Direction,
    pub iteration:u32,
    pub warmup:bool,
    pub selected_keys:Vec<String>,
    pub emitted_keys:Vec<String>,
    pub accepted_winner_keys:Option<Vec<String>>,
    pub affected_keys:Vec<String>,
    pub dispatch_keys:Option<Vec<String>>,
    pub failed_operations:u64,
    pub pending_operations:u64,
    pub observation:NativeObservation,
    pub timings:NativeTimings,
}

impl RoutineSample {
    pub fn validate(&self)->Result<(),String> {
        if !AVAILABLE.contains(&self.scenario) {return Err("scenario has no verified native routine adapter".into());}
        self.observation.validate_routine_invariants()?;
        validate_intents(&self.observation)?;
        if self.observation.units_visited==0 || self.observation.hash_totals()?.calls==0 || self.observation.requests==0 {
            return Err("fixed routine lacks positive native visit, SHA or transport observation".into());
        }
        if self.failed_operations!=0 || self.pending_operations!=0 || self.selected_keys.is_empty()
            || self.selected_keys!=self.emitted_keys || self.affected_keys!=self.emitted_keys {
            return Err("native routine key settlement is incomplete".into());
        }
        for keys in [&self.selected_keys,&self.emitted_keys,&self.affected_keys]
            .into_iter().chain(self.accepted_winner_keys.iter()).chain(self.dispatch_keys.iter()) {
            if keys.windows(2).any(|pair|pair[0]>=pair[1]) {return Err("key evidence is not a unique ordered set".into());}
        }
        if self.accepted_winner_keys.as_ref().is_some_and(|keys| keys!=&self.emitted_keys)
            || self.dispatch_keys.as_ref().is_some_and(|keys| keys!=&self.selected_keys)
            || self.accepted_winner_keys.is_some()==self.dispatch_keys.is_some() {
            return Err("fixed routine transport key witness differs from settled keys".into());
        }
        let t=&self.timings;
        if [t.durable_save_ms,t.publication_complete_ms,t.receiver_durable_ack_ms]
            .iter().any(|v|!v.is_finite() || *v<0.0)
            || t.durable_save_ms>t.publication_complete_ms || t.publication_complete_ms>t.receiver_durable_ack_ms {
            return Err("invalid cumulative native phase timing".into());
        }
        Ok(())
    }
}

fn validate_intents(observation:&NativeObservation)->Result<(),String> {
        if !observation.receive_intents.as_ref().is_some_and(|v|v.complete)
            || !observation.commit_intents.as_ref().is_some_and(|v|v.complete) {
            return Err("exact intent provenance is absent".into());
        }
        for (name,evidence) in [("native/native_receive_intent",observation.receive_intents.as_ref().unwrap()),
            ("native/native_intent",observation.commit_intents.as_ref().unwrap())] {
            let domain=observation.hashes.get(name).cloned().unwrap_or_default();
            let bytes=evidence.inputs.iter().try_fold(0u64,|total,input|total.checked_add(input.input_bytes)).ok_or("intent length overflow")?;
            if !evidence.errors.is_empty() || evidence.observed_calls!=domain.calls || evidence.observed_bytes!=domain.bytes
                || evidence.captured_calls!=domain.calls || evidence.captured_bytes!=domain.bytes
                || evidence.inputs.len() as u64!=domain.calls || bytes!=domain.bytes {
                return Err("exact intent calls/bytes do not conserve actual SHA work".into());
            }
        }
        Ok(())
}

#[derive(Debug,Serialize)]
pub struct CycleFailure {
    pub reason:String,
    pub observation:Option<Box<NativeObservation>>,
}
impl From<String> for CycleFailure {
    fn from(reason:String)->Self {Self {reason,observation:None}}
}
impl From<&str> for CycleFailure {
    fn from(reason:&str)->Self {reason.to_owned().into()}
}
#[derive(Debug,Serialize)]
pub struct FailedSlot {pub scenario:Scenario,pub direction:Direction,pub iteration:u32,pub failure:CycleFailure}
#[derive(Debug,Serialize)]
pub struct CollectedSamples {pub samples:Vec<RoutineSample>,pub failures:Vec<FailedSlot>}

/// Setup, certification and post-scope correctness checks belong to the actual adapter.
/// The callback returns every observed scope, including warmups; it never returns estimated work.
pub fn collect(mut cycle:impl FnMut(Scenario,Direction,u32,bool)->Result<RoutineSample,CycleFailure>)
    ->CollectedSamples {
    let mut result=CollectedSamples {samples:Vec::new(),failures:Vec::new()};
    for &scenario in AVAILABLE {
        for direction in [Direction::AtoB,Direction::BtoA] {
            for iteration in 0..WARMUPS+REPETITIONS {
                let warmup=iteration<WARMUPS;
                let sample=match cycle(scenario,direction,iteration,warmup) {
                    Ok(sample)=>sample,
                    Err(failure)=>{result.failures.push(FailedSlot {scenario,direction,iteration,failure});return result;},
                };
                let validity=sample.validate();
                if sample.scenario!=scenario || sample.direction!=direction || sample.iteration!=iteration
                    || sample.warmup!=warmup {
                    result.failures.push(FailedSlot {scenario,direction,iteration,failure:"adapter returned the wrong fixed matrix slot".into()});
                    result.samples.push(sample);return result;
                }
                result.samples.push(sample);
                if let Err(reason)=validity {result.failures.push(FailedSlot {scenario,direction,iteration,failure:reason.into()});return result;}
            }
        }
    }
    result
}

pub trait AsyncCycleDriver {
    fn cycle(&mut self,scenario:Scenario,direction:Direction,iteration:u32,warmup:bool)
        ->impl std::future::Future<Output=Result<RoutineSample,CycleFailure>>;
}
pub async fn collect_async(driver:&mut impl AsyncCycleDriver)->CollectedSamples {
    let mut collection=CollectedSamples {samples:Vec::new(),failures:Vec::new()};
    for &scenario in AVAILABLE {
        for direction in [Direction::AtoB,Direction::BtoA] {
            for iteration in 0..WARMUPS+REPETITIONS {
                let warmup=iteration<WARMUPS;
                let sample=match driver.cycle(scenario,direction,iteration,warmup).await {
                    Ok(sample)=>sample,
                    Err(failure)=>{collection.failures.push(FailedSlot {scenario,direction,iteration,failure});return collection;},
                };
                let valid=sample.validate();
                let matches=(sample.scenario,sample.direction,sample.iteration,sample.warmup)==(scenario,direction,iteration,warmup);
                collection.samples.push(sample);
                if !matches || valid.is_err() {
                    collection.failures.push(FailedSlot {scenario,direction,iteration,
                        failure:valid.err().unwrap_or_else(||"adapter returned the wrong fixed matrix slot".into()).into()});
                    return collection;
                }
            }
        }
    }
    collection
}

#[derive(Debug,Serialize)]
pub struct NativeSummary {
    pub scenario:Scenario,
    pub direction:Direction,
    pub durable_save_ms:Percentiles,
    pub publication_complete_ms:Percentiles,
    pub receiver_durable_ack_ms:Percentiles,
    pub work:NativeWorkSummary,
}

#[derive(Debug,Serialize)]
pub struct WorkDistribution {pub count:usize,pub minimum:u64,pub maximum:u64,pub sum:u64,pub mean:f64,pub percentiles:Percentiles}
fn work_distribution(values:impl Iterator<Item=u64>)->Result<WorkDistribution,String> {
    let values=values.collect::<Vec<_>>();
    if values.is_empty() || values.iter().any(|v|*v>(1u64<<53)) {return Err("work distribution is empty or cannot be represented exactly".into());}
    let sum=values.iter().try_fold(0u64,|sum,v|sum.checked_add(*v)).ok_or("work sum overflow")?;
    Ok(WorkDistribution {count:values.len(),minimum:*values.iter().min().unwrap(),maximum:*values.iter().max().unwrap(),
        sum,mean:sum as f64/values.len() as f64,percentiles:percentiles(values.iter().map(|v|*v as f64))?})
}
#[derive(Debug,Serialize)]
pub struct NativeWorkSummary {
    pub units_visited:WorkDistribution,
    pub complete_sha_calls:WorkDistribution,
    pub complete_sha_input_bytes:WorkDistribution,
    pub request_attempts:WorkDistribution,
    pub uploaded_bytes:WorkDistribution,
    pub downloaded_bytes:WorkDistribution,
    pub partial_capture_messages_read:WorkDistribution,
    pub partial_capture_message_bytes_read:WorkDistribution,
    pub partial_capture_pages_written:WorkDistribution,
    pub partial_capture_pages_reused:WorkDistribution,
    pub control_body_reads:WorkDistribution,
    pub control_body_bytes_read:WorkDistribution,
}
fn summarize_work(samples:&[&RoutineSample])->Result<NativeWorkSummary,String> {
    let hashes=samples.iter().map(|sample|sample.observation.hash_totals()).collect::<Result<Vec<_>,_>>()?;
    Ok(NativeWorkSummary {
        units_visited:work_distribution(samples.iter().map(|s|s.observation.units_visited))?,
        complete_sha_calls:work_distribution(hashes.iter().map(|h|h.calls))?,
        complete_sha_input_bytes:work_distribution(hashes.iter().map(|h|h.bytes))?,
        request_attempts:work_distribution(samples.iter().map(|s|s.observation.requests))?,
        uploaded_bytes:work_distribution(samples.iter().map(|s|s.observation.uploaded_bytes))?,
        downloaded_bytes:work_distribution(samples.iter().map(|s|s.observation.downloaded_bytes))?,
        partial_capture_messages_read:work_distribution(samples.iter().map(|s|s.observation.messages_read))?,
        partial_capture_message_bytes_read:work_distribution(samples.iter().map(|s|s.observation.message_bytes_read))?,
        partial_capture_pages_written:work_distribution(samples.iter().map(|s|s.observation.pages_written))?,
        partial_capture_pages_reused:work_distribution(samples.iter().map(|s|s.observation.pages_reused))?,
        control_body_reads:work_distribution(samples.iter().map(|s|s.observation.body_control_work.read_operations))?,
        control_body_bytes_read:work_distribution(samples.iter().map(|s|s.observation.body_control_work.read_bytes))?,
    })
}

#[derive(Debug,Serialize)]
pub struct NativeResumeSample {
    pub direction:Direction,
    pub iteration:u32,
    pub warmup:bool,
    pub reopened_ms:f64,
    pub receiver_durable_ack_ms:f64,
    pub missed_published_keys:Vec<String>,
    pub affected_keys:Vec<String>,
    pub observation:NativeObservation,
}
impl NativeResumeSample {
    pub fn validate(&self)->Result<(),String> {
        self.observation.validate_routine_invariants()?;
        validate_intents(&self.observation)?;
        if self.missed_published_keys.is_empty() || self.missed_published_keys!=self.affected_keys
            || self.missed_published_keys.windows(2).any(|p|p[0]>=p[1])
            || !self.reopened_ms.is_finite() || !self.receiver_durable_ack_ms.is_finite()
            || self.reopened_ms<0.0 || self.reopened_ms>self.receiver_durable_ack_ms {
            return Err("invalid native reopen/receive evidence".into());
        }
        Ok(())
    }
}
#[derive(Debug,Serialize)]
pub struct NativeResumeSummary {pub direction:Direction,pub reopened_ms:Percentiles,pub receiver_durable_ack_ms:Percentiles}
pub fn summarize_resume(samples:&[NativeResumeSample])->Result<Vec<NativeResumeSummary>,String> {
    if samples.len()!=2*(WARMUPS+REPETITIONS) as usize {return Err("incomplete native resume matrix".into());}
    let mut summaries=Vec::new();let mut index=0;
    for direction in [Direction::AtoB,Direction::BtoA] {
        for iteration in 0..WARMUPS+REPETITIONS {
            let sample=&samples[index];index+=1;sample.validate()?;
            if (sample.direction,sample.iteration,sample.warmup)!=(direction,iteration,iteration<WARMUPS) {
                return Err("reordered native resume matrix".into());
            }
        }
        let recorded=samples.iter().filter(|s|s.direction==direction && !s.warmup).collect::<Vec<_>>();
        summaries.push(NativeResumeSummary {direction,reopened_ms:percentiles(recorded.iter().map(|s|s.reopened_ms))?,
            receiver_durable_ack_ms:percentiles(recorded.iter().map(|s|s.receiver_durable_ack_ms))?});
    }
    Ok(summaries)
}

pub fn compare_resume(target_certificate:&ScaleCertificate,above_certificate:&ScaleCertificate,
    target:&[NativeResumeSample],above:&[NativeResumeSample])->Result<Vec<PairedProof>,String> {
    validate_above_target(target_certificate,above_certificate)?;
    summarize_resume(target)?;summarize_resume(above)?;
    target.iter().zip(above).map(|(a,b)| {
        if (a.direction,a.iteration,a.warmup)!=(b.direction,b.iteration,b.warmup)
            || a.missed_published_keys!=b.missed_published_keys || a.affected_keys!=b.affected_keys {
            return Err("resume scale pair changed actual missed/affected keys".into());
        }
        Ok(PairedProof {scenario:Scenario::OrdinaryResume,direction:a.direction,iteration:a.iteration,warmup:a.warmup,
            proof:assert_frozen_growth(&a.observation,&b.observation)?})
    }).collect()
}

#[derive(Debug,Serialize)]
pub struct MilestoneCoverage {
    pub scenario:Scenario,
    pub native_routine_available:bool,
    pub renderer_completion_ms:Option<f64>,
    pub library_usable_ms:Option<f64>,
    pub all_bodies_local_ms:Option<f64>,
    pub required_endpoint:&'static str,
}

#[derive(Debug,Serialize)]
pub struct NativeTierReport {
    pub schema:&'static str,
    pub checkpoint:String,
    pub binary_sha256:String,
    pub transport:String,
    pub certificate:ScaleCertificate,
    pub timing_origin:&'static str,
    pub hash_scope:&'static str,
    pub capture_scope:&'static str,
    pub fixed_changes:NativeFixedChanges,
    pub work_acceptance:&'static str,
    pub warmups:u32,
    pub repetitions:u32,
    pub samples:Vec<RoutineSample>,
    pub summaries:Vec<NativeSummary>,
    pub milestones:Vec<MilestoneCoverage>,
}

#[derive(Debug,Serialize)]
pub struct NativeFixedChanges {
    pub initial_conversation_messages:u64,
    pub middle_index:u64,
    pub burst_durable_commits:u32,
    pub oversized_text_prefix_bytes:u64,
    pub message_identity:&'static str,
    pub setup_boundary:&'static str,
}

pub fn report(checkpoint:&str,binary_sha256:&str,transport:&str,certificate:ScaleCertificate,
    collection:&CollectedSamples)->Result<NativeTierReport,String> {
    if !collection.failures.is_empty() {return Err("native collection contains failed or incomplete samples".into());}
    let samples=collection.samples.clone();
    certificate.validate()?;
    if checkpoint.len()!=40 || !checkpoint.bytes().all(|b|b.is_ascii_hexdigit())
        || binary_sha256.len()!=64 || !binary_sha256.bytes().all(|b|b.is_ascii_hexdigit()) {
        return Err("exact source and executable identities are required".into());
    }
    if samples.len()!=AVAILABLE.len()*2*(WARMUPS+REPETITIONS) as usize {return Err("incomplete fixed sample matrix".into());}
    let mut index=0;
    let mut summaries=Vec::new();
    for &scenario in AVAILABLE {
        for direction in [Direction::AtoB,Direction::BtoA] {
            for iteration in 0..WARMUPS+REPETITIONS {
                let sample=&samples[index]; index+=1;
                sample.validate()?;
                if (sample.scenario,sample.direction,sample.iteration,sample.warmup)
                    !=(scenario,direction,iteration,iteration<WARMUPS) {return Err("missing, duplicate or reordered matrix slot".into());}
            }
            let recorded=samples.iter().filter(|s|s.scenario==scenario && s.direction==direction && !s.warmup).collect::<Vec<_>>();
            summaries.push(NativeSummary {scenario,direction,
                durable_save_ms:percentiles(recorded.iter().map(|s|s.timings.durable_save_ms))?,
                publication_complete_ms:percentiles(recorded.iter().map(|s|s.timings.publication_complete_ms))?,
                receiver_durable_ack_ms:percentiles(recorded.iter().map(|s|s.timings.receiver_durable_ack_ms))?,
                work:summarize_work(&recorded)?});
        }
    }
    let milestones=ALL_SCENARIOS.iter().map(|&scenario|MilestoneCoverage {scenario,
        native_routine_available:AVAILABLE.contains(&scenario),renderer_completion_ms:None,
        library_usable_ms:None,all_bodies_local_ms:None,
        required_endpoint:if AVAILABLE.contains(&scenario) {"renderer paint and production scheduler completion"}
            else {"verified production scenario, source-reader observations and actual completion receipt"}}).collect();
    Ok(NativeTierReport {schema:"risunest.lww-final-native-tier/v1",checkpoint:checkpoint.into(),binary_sha256:binary_sha256.into(),
        transport:transport.into(),certificate,timing_origin:"native commit start, cumulative durable publication and receiver ACK; excludes renderer",
        hash_scope:"complete observed content-identity SHA inputs; excludes server internals, MAC, HKDF and renderer",
        capture_scope:"partial actual native message capture; full observed SHA inputs remain separately reported",
        fixed_changes:NativeFixedChanges {initial_conversation_messages:4096,middle_index:2048,burst_durable_commits:16,
            oversized_text_prefix_bytes:128*1024,message_identity:"original IDs retained; inserted IDs contain actual direction and unique iteration",
            setup_boundary:"native construction, physical certification, binding and both-endpoint bootstrap drain excluded before any warmup"},
        work_acceptance:"zero unrelated-scale unit/request/SHA-call growth; exact selected/emitted/affected sets; SHA bytes only two exact proven scalar leaves within aggregate8192; complete body scopes and zero asset/head/proof IO",
        warmups:WARMUPS,repetitions:REPETITIONS,samples,summaries,milestones})
}

#[derive(Debug,Serialize)]
pub struct PairedProof {pub scenario:Scenario,pub direction:Direction,pub iteration:u32,pub warmup:bool,pub proof:FrozenGrowthProof}

pub fn compare(target:&NativeTierReport,above:&NativeTierReport)->Result<Vec<PairedProof>,String> {
    validate_above_target(&target.certificate,&above.certificate)?;
    report(&target.checkpoint,&target.binary_sha256,&target.transport,target.certificate.clone(),
        &CollectedSamples {samples:target.samples.clone(),failures:Vec::new()})?;
    report(&above.checkpoint,&above.binary_sha256,&above.transport,above.certificate.clone(),
        &CollectedSamples {samples:above.samples.clone(),failures:Vec::new()})?;
    if target.transport!=above.transport || target.checkpoint!=above.checkpoint || target.binary_sha256!=above.binary_sha256
        || target.samples.len()!=above.samples.len() {return Err("unpaired source, transport or sample matrix".into());}
    target.samples.iter().zip(&above.samples).map(|(a,b)| {
        a.validate()?; b.validate()?;
        if (a.scenario,a.direction,a.iteration,a.warmup)!=(b.scenario,b.direction,b.iteration,b.warmup)
            || a.selected_keys!=b.selected_keys || a.emitted_keys!=b.emitted_keys || a.affected_keys!=b.affected_keys
            || a.dispatch_keys!=b.dispatch_keys || a.accepted_winner_keys!=b.accepted_winner_keys {
            return Err("scale pair changed exact native keys or matrix slot".into());
        }
        Ok(PairedProof {scenario:a.scenario,direction:a.direction,iteration:a.iteration,warmup:a.warmup,
            proof:assert_frozen_growth(&a.observation,&b.observation)?})
    }).collect()
}

pub fn write_comparison(directory:&std::path::Path,target:&NativeTierReport,above:&NativeTierReport)->Result<(),String> {
    let proof=compare(target,above);
    std::fs::create_dir_all(directory).map_err(|e|e.to_string())?;
    if directory.join("native-final-pair.json").exists() || directory.join("INVALID-native-final-pair.json").exists() {
        return Err("native final comparison output already exists".into());
    }
    let (name,value)=match &proof {
        Ok(proofs)=>("native-final-pair.json",serde_json::json!({"status":"native-only-valid","target":target,"above":above,"proofs":proofs})),
        Err(reason)=>("INVALID-native-final-pair.json",serde_json::json!({"status":"INVALID","reason":reason,"target":target,"above":above})),
    };
    let file=std::fs::OpenOptions::new().write(true).create_new(true).open(directory.join(name)).map_err(|e|e.to_string())?;
    serde_json::to_writer_pretty(file,&value).map_err(|e|e.to_string())?;
    proof.map(|_|())
}

pub fn write_invalid_collection(directory:&std::path::Path,collection:&CollectedSamples,reason:&str)->Result<(),String> {
    std::fs::create_dir_all(directory).map_err(|e|e.to_string())?;
    let file=std::fs::OpenOptions::new().write(true).create_new(true).open(directory.join("INVALID-native-final-collection.json"))
        .map_err(|e|e.to_string())?;
    serde_json::to_writer_pretty(file,&serde_json::json!({"status":"INVALID","reason":reason,"actual_observations":collection}))
        .map_err(|e|e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn certificate()->ScaleCertificate {
        serde_json::from_value(serde_json::json!({"schema":"risunest.native-persisted-scale/v1",
            "requirement":{"tier":"correctness","minimum_database_bytes":4096,"minimum_assets":1},
            "database":{"page_size":4096,"page_count":2,"freelist_count":0,"file_bytes":8192,"wal_bytes":0,"regular_file":true,
            "sparse_file":false,"reparse_point":false,"checkpoint_complete":true,"reopened":true,"integrity_checked":true,"native_schema_only":true,
            "active_characters":2,"active_conversations":2,"active_messages":2,"active_record_bytes":200,"maximum_message_bytes":100,
            "maximum_character_detail_bytes":100,"maximum_root_bytes":100},"catalog_rows":1,"verified_owner_heads":1,
            "measured_conversation_sha256":"ab".repeat(32),"assets":[{"payload_hash":"cd".repeat(32),"catalog_bytes":1024,"file_bytes":1024,
                "regular_file":true,"sparse_file":false,"reparse_point":false,"verified_file_hash":"cd".repeat(32),"active_aliases":1,"verified_owner_references":1}]})).unwrap()
    }
    fn sample(scenario:Scenario,direction:Direction,iteration:u32,warmup:bool)->RoutineSample {
        let evidence=super::super::invalid_diagnostics::ReceiveIntentEvidence {observed_calls:0,observed_bytes:0,
            captured_calls:0,captured_bytes:0,complete:true,errors:Vec::new(),inputs:Vec::new()};
        let mut observation=NativeObservation {units_visited:1,requests:1,body_scope_complete:Some(true),asset_body_opens:Some(0),
            asset_body_bytes_read:Some(0),shared_head_reads:Some(0),receive_intents:Some(evidence.clone()),commit_intents:Some(evidence),
            ..Default::default()};
        observation.insert_domain("unit-test","synthetic-input",1,1);
        let durable=if warmup {999.0} else {iteration as f64};
        RoutineSample {scenario,direction,iteration,warmup,selected_keys:vec!["synthetic-key".into()],emitted_keys:vec!["synthetic-key".into()],
            affected_keys:vec!["synthetic-key".into()],accepted_winner_keys:Some(vec!["synthetic-key".into()]),dispatch_keys:None,
            failed_operations:0,pending_operations:0,observation,
            timings:NativeTimings {durable_save_ms:durable,publication_complete_ms:durable+1.0,receiver_durable_ack_ms:durable+2.0}}
    }
    #[test]
    #[ignore = "measurement harness self-test; benchmarks/lww-native/run.ps1 runs it"]
    fn transport_key_witness_requires_the_exact_actual_fixed_change_set() {
        let original=sample(Scenario::SettingEdit,Direction::AtoB,0,false);
        original.validate().unwrap();
        for keys in [Vec::new(),vec!["extra".into()],vec!["extra".into(),"synthetic-key".into()]] {
            let mut changed=original.clone();changed.accepted_winner_keys=Some(keys.clone());
            assert!(changed.validate().is_err());
            changed.accepted_winner_keys=None;changed.dispatch_keys=Some(keys);
            assert!(changed.validate().is_err());
        }
        let mut external=original.clone();external.accepted_winner_keys=None;
        external.dispatch_keys=Some(external.selected_keys.clone());external.validate().unwrap();
        external.dispatch_keys=None;assert!(external.validate().is_err());
        external.accepted_winner_keys=original.accepted_winner_keys.clone();
        external.dispatch_keys=Some(external.selected_keys.clone());assert!(external.validate().is_err());
    }

    #[test]
    #[ignore = "measurement harness self-test; benchmarks/lww-native/run.ps1 runs it"]
    fn fixed_policy_retains_warmups_but_excludes_them_from_percentiles() {
        let collection=collect(|s,d,i,w|Ok(sample(s,d,i,w)));
        let report=report(&"a".repeat(40),&"b".repeat(64),"test adapter",certificate(),&collection).unwrap();
        assert_eq!(report.samples.len(),AVAILABLE.len()*2*35);
        assert!(report.summaries.iter().all(|s|s.durable_save_ms.count==30 && s.durable_save_ms.p99<100.0));
        assert_eq!(report.milestones.len(),ALL_SCENARIOS.len());
        assert!(report.milestones.iter().all(|m|m.renderer_completion_ms.is_none() && m.library_usable_ms.is_none() && m.all_bodies_local_ms.is_none()));
        let directory=tempfile::tempdir().unwrap();
        assert!(write_comparison(directory.path(),&report,&report).is_err());
        assert!(directory.path().join("INVALID-native-final-pair.json").is_file());
        assert!(!directory.path().join("native-final-pair.json").exists());
        let mut missing=collection;missing.samples.pop();
        assert!(super::report(&"a".repeat(40),&"b".repeat(64),"test adapter",certificate(),&missing).is_err());
        missing.samples.push(sample(AVAILABLE[0],Direction::AtoB,0,true));
        assert!(super::report(&"a".repeat(40),&"b".repeat(64),"test adapter",certificate(),&missing).is_err());
    }
    #[test]
    #[ignore = "measurement harness self-test; benchmarks/lww-native/run.ps1 runs it"]
    fn invalid_scope_and_driver_failure_retain_actual_observations() {
        let failed=collect(|s,d,i,w| {let mut sample=sample(s,d,i,w);sample.observation.body_scope_complete=Some(false);Ok(sample)});
        assert_eq!(failed.samples.len(),1);assert_eq!(failed.failures.len(),1);
        let mut calls=0;
        let failed=collect(|s,d,i,w| {calls+=1;
            if calls==2 {return Err(CycleFailure {reason:"failed actual network call".into(),observation:Some(Box::new(sample(s,d,i,w).observation))});}
            Ok(sample(s,d,i,w))});
        assert_eq!(failed.samples.len(),1);assert_eq!(failed.failures.len(),1);
        assert!(failed.failures[0].failure.observation.is_some());
        let mut malformed=sample(Scenario::SettingEdit,Direction::AtoB,0,true);
        malformed.observation.commit_intents.as_mut().unwrap().captured_calls=1;
        assert!(malformed.validate().unwrap_err().contains("conserve"));
        let mut unobserved=sample(Scenario::SettingEdit,Direction::AtoB,0,true);
        unobserved.observation.units_visited=0;assert!(unobserved.validate().is_err());
    }
    #[test]
    #[ignore = "measurement harness self-test; benchmarks/lww-native/run.ps1 runs it"]
    fn native_resume_summary_has_complete_fixed_matrix_and_no_renderer_phase() {
        let mut samples=Vec::new();
        for direction in [Direction::AtoB,Direction::BtoA] {
            for iteration in 0..WARMUPS+REPETITIONS {
                let warmup=iteration<WARMUPS;
                samples.push(NativeResumeSample {direction,iteration,warmup,reopened_ms:if warmup {999.0} else {1.0},
                    receiver_durable_ack_ms:if warmup {1000.0} else {2.0},missed_published_keys:vec!["synthetic-key".into()],
                    affected_keys:vec!["synthetic-key".into()],observation:sample(Scenario::SettingEdit,direction,iteration,warmup).observation});
            }
        }
        let summary=summarize_resume(&samples).unwrap();
        assert!(summary.iter().all(|s|s.reopened_ms.count==30 && s.reopened_ms.p99==1.0));
        assert!(compare_resume(&certificate(),&certificate(),&samples,&samples).is_err());
        samples[0].affected_keys.clear();assert!(summarize_resume(&samples).is_err());
    }
    #[test]
    #[ignore = "measurement harness self-test; benchmarks/lww-native/run.ps1 runs it"]
    fn asynchronous_runner_uses_the_same_fixed_slots_and_failure_retention() {
        struct Driver;
        impl AsyncCycleDriver for Driver {
            async fn cycle(&mut self,s:Scenario,d:Direction,i:u32,w:bool)->Result<RoutineSample,CycleFailure> {
                Ok(sample(s,d,i,w))
            }
        }
        let mut driver=Driver;
        let mut future=std::pin::pin!(collect_async(&mut driver));
        let mut context=std::task::Context::from_waker(std::task::Waker::noop());
        let std::task::Poll::Ready(collection)=std::future::Future::poll(future.as_mut(),&mut context) else {panic!("fixture futures are immediately ready")};
        assert_eq!(collection.samples.len(),AVAILABLE.len()*2*35);assert!(collection.failures.is_empty());
        let report=report(&"a".repeat(40),&"b".repeat(64),"test async adapter",certificate(),&collection).unwrap();
        assert_eq!(report.summaries.len(),AVAILABLE.len()*2);
    }
    #[test]
    #[ignore = "measurement harness self-test; benchmarks/lww-native/run.ps1 runs it"]
    fn collector_rejects_incomplete_scopes_and_unavailable_scenarios() {
        let failed=collect(|_,_,_,_|Err("actual adapter failed".into()));
        assert_eq!(failed.failures.len(),1);
        assert!(failed.failures[0].failure.reason.contains("actual adapter"));
        let directory=tempfile::tempdir().unwrap();
        write_invalid_collection(directory.path(),&failed,"actual adapter failed").unwrap();
        assert!(directory.path().join("INVALID-native-final-collection.json").is_file());
        assert!(!directory.path().join("native-final-pair.json").exists());
        assert!(write_invalid_collection(directory.path(),&failed,"repeat").is_err());
        assert!(!AVAILABLE.contains(&Scenario::GenerationCompletion));
        assert!(!AVAILABLE.contains(&Scenario::OrdinaryResume));
        assert_eq!((WARMUPS,REPETITIONS),(5,30));
    }
    #[test]
    #[ignore = "measurement harness self-test; benchmarks/lww-native/run.ps1 runs it"]
    fn above_validator_is_required_before_native_comparison() {
        // A correctness certificate cannot stand in for either final tier.
        let value=serde_json::json!({"schema":"risunest.native-persisted-scale/v1","requirement":{"tier":"correctness","minimum_database_bytes":0,"minimum_assets":0},
            "database":{"page_size":4096,"page_count":0,"freelist_count":0,"file_bytes":0,"wal_bytes":0,"regular_file":true,
            "sparse_file":false,"reparse_point":false,"checkpoint_complete":true,"reopened":true,"integrity_checked":true,"native_schema_only":true,
            "active_characters":0,"active_conversations":0,"active_messages":0,"active_record_bytes":0,"maximum_message_bytes":0,
            "maximum_character_detail_bytes":0,"maximum_root_bytes":0},"catalog_rows":0,"verified_owner_heads":0,
            "measured_conversation_sha256":"ab".repeat(32),"assets":[]});
        let certificate:ScaleCertificate=serde_json::from_value(value).unwrap();
        assert!(validate_above_target(&certificate,&certificate).unwrap_err().contains("target and above-target"));
    }
}
