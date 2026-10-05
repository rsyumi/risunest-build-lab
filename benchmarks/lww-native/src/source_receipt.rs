use serde::{Deserialize,Serialize};
use std::collections::BTreeMap;

#[derive(Clone,Debug,Default,Deserialize,Serialize,PartialEq,Eq)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
pub struct Work {
    pub open_attempts:u64,pub opens:u64,pub failed_opens:u64,
    pub read_calls:u64,pub read_bytes:u64,pub failed_reads:u64,pub unknown_read_results:u64,
    pub outstanding_readers:u64,pub hash_calls:u64,pub hash_bytes:u64,pub hash_finalizations:u64,pub failed_hashes:u64,
}
impl Work {
    fn add(&mut self,other:&Self)->Result<(),String> {
        macro_rules! fields {($($name:ident),*)=>{$(self.$name=self.$name.checked_add(other.$name).ok_or("source counter overflow")?;)*}}
        fields!(open_attempts,opens,failed_opens,read_calls,read_bytes,failed_reads,unknown_read_results,
            outstanding_readers,hash_calls,hash_bytes,hash_finalizations,failed_hashes);
        Ok(())
    }
    pub fn complete(&self)->bool {
        self.failed_opens==0 && self.failed_reads==0 && self.unknown_read_results==0 && self.outstanding_readers==0
            && self.failed_hashes==0 && self.open_attempts==self.opens && self.hash_calls==self.hash_finalizations
    }
    pub fn asset_zero(&self)->bool {*self==Self::default()}
}
#[derive(Clone,Debug,Deserialize,Serialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
pub struct ReadRange {pub start:u64,pub bytes:u64,pub calls:u64}
#[derive(Clone,Debug,Deserialize,Serialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
pub struct ObjectWork {
    pub hash:String,pub purposes:Vec<String>,pub placement:String,pub flow:String,pub work:Work,
    pub read_ranges:Vec<ReadRange>,pub selected_ranges:Vec<ReadRange>,pub hash_domains:BTreeMap<String,Work>,
}
#[derive(Clone,Debug,Default,Deserialize,Serialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
pub struct PendingBodyWork {pub upload_jobs:u64,pub download_jobs:u64,pub upload_sessions:u64}
#[derive(Clone,Debug,Deserialize,Serialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
pub struct ReadBarrierReached {
    #[serde(rename="type")] pub event_type:String,
    pub scope_id:String,pub generation:u64,pub barrier_id:String,pub hash:String,pub flow:String,pub phase:String,
    pub placement:String,pub read_kind:String,pub offset:u64,pub elapsed_nanos:String,
}
#[derive(Clone,Debug,Deserialize,Serialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
pub struct ReadBarrier {
    pub scope_id:String,pub generation:u64,pub barrier_id:String,pub hash:String,pub flow:String,pub status:String,
    pub phase:String,pub waiters:u64,pub reached:Option<ReadBarrierReached>,
    pub released_elapsed_nanos:Option<String>,pub cancellation_reason:Option<String>,
}
fn nullable_barrier<'de,D:serde::Deserializer<'de>>(deserializer:D)->Result<Option<ReadBarrier>,D::Error> {
    Option::<ReadBarrier>::deserialize(deserializer)
}
fn decimal_nanos(value:&str)->Result<u128,String> {
    let parsed=value.parse::<u128>().map_err(|_|"noncanonical source barrier clock")?;
    if parsed.to_string()!=value {return Err("noncanonical source barrier clock".into());}Ok(parsed)
}
#[derive(Clone,Debug,Deserialize,Serialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
pub struct Snapshot {
    pub scope_id:String,pub generation:u64,pub phase:String,pub role_count:u64,pub complete:bool,
    pub objects:Vec<ObjectWork>,pub placements:BTreeMap<String,Work>,pub purposes:BTreeMap<String,Work>,
    pub flows:BTreeMap<String,Work>,pub hash_domains:BTreeMap<String,Work>,pub total:Work,pub unknown_work:Work,
    pub violations:BTreeMap<String,u64>,pub pending_workers:u64,pub pending_requests:u64,pub pending_body_work:PendingBodyWork,
    #[serde(deserialize_with="nullable_barrier")]
    pub read_barrier:Option<ReadBarrier>,
}
fn valid_hash(hash:&str)->bool {hash.len()==64 && hash.bytes().all(|b|b.is_ascii_digit() || (b'a'..=b'f').contains(&b))}
fn accumulate(map:&mut BTreeMap<String,Work>,key:&str,work:&Work)->Result<(),String> {
    map.entry(key.to_owned()).or_default().add(work)
}
impl Snapshot {
    pub fn validate(&self)->Result<(),String> {
        if !self.complete || self.scope_id.is_empty() || self.generation==0 || self.phase.is_empty()
            || !self.total.complete() || self.unknown_work!=Work::default()
            || self.violations.values().any(|&v|v!=0) || self.pending_workers!=0 || self.pending_requests!=0
            || self.pending_body_work.upload_jobs!=0 || self.pending_body_work.download_jobs!=0
            || self.pending_body_work.upload_sessions!=0 {return Err("source scope incomplete".into());}
        let mut total=Work::default();let mut placements=BTreeMap::new();let mut purposes=BTreeMap::new();
        let mut flows=BTreeMap::new();let mut hashes=BTreeMap::new();let mut rows=std::collections::BTreeSet::new();
        for object in &self.objects {
            if !valid_hash(&object.hash) || object.placement.is_empty() || object.flow.is_empty() || !object.work.complete()
                || !rows.insert((&object.hash,&object.placement,&object.flow))
                || object.purposes.is_empty() || object.purposes.windows(2).any(|p|p[0]>=p[1])
                || object.purposes.iter().any(|p|p!="Asset" && p!="Control") {return Err("source object identity or purpose invalid".into());}
            total.add(&object.work)?;accumulate(&mut placements,&object.placement,&object.work)?;
            accumulate(&mut flows,&object.flow,&object.work)?;
            let role=if object.purposes.iter().any(|p|p=="Asset") {"Asset"} else {"Control"};
            accumulate(&mut purposes,role,&object.work)?;
            let mut hash_calls=0u64;let mut hash_bytes=0u64;let mut finalizations=0u64;
            for (name,work) in &object.hash_domains {
                if name.is_empty() || !work.complete() {return Err("source hash domain incomplete".into());}
                hash_calls=hash_calls.checked_add(work.hash_calls).ok_or("source hash overflow")?;
                hash_bytes=hash_bytes.checked_add(work.hash_bytes).ok_or("source hash overflow")?;
                finalizations=finalizations.checked_add(work.hash_finalizations).ok_or("source hash overflow")?;
                accumulate(&mut hashes,name,work)?;
            }
            if (hash_calls,hash_bytes,finalizations)!=(object.work.hash_calls,object.work.hash_bytes,object.work.hash_finalizations) {
                return Err("source per-object SHA conservation failed".into());
            }
            let mut read_calls=0u64;let mut read_bytes=0u64;
            for range in &object.read_ranges {
                range.start.checked_add(range.bytes).ok_or("source range overflow")?;
                if range.calls==0 || range.bytes==0 {return Err("source positive read range invalid".into());}
                read_calls=read_calls.checked_add(range.calls).ok_or("source read overflow")?;
                read_bytes=read_bytes.checked_add(range.bytes).ok_or("source read overflow")?;
            }
            for selected in &object.selected_ranges {
                selected.start.checked_add(selected.bytes).ok_or("source selected range overflow")?;
                if selected.calls==0 {return Err("source selected range invalid".into());}
            }
            // SQL extraction has no file ranges; file EOF calls have no positive-byte range.
            if read_calls>object.work.read_calls || (object.placement=="inline" && !object.read_ranges.is_empty())
                || (object.placement!="inline" && read_bytes!=object.work.read_bytes) {
                return Err("source positive range conservation failed".into());
            }
        }
        if total!=self.total || placements!=self.placements || purposes!=self.purposes || flows!=self.flows || hashes!=self.hash_domains {
            return Err("source aggregate conservation failed".into());
        }
        if let Some(barrier)=&self.read_barrier {
            let reached=barrier.reached.as_ref().ok_or("source read barrier never reached physical reader")?;
            if barrier.scope_id!=self.scope_id || barrier.generation!=self.generation || barrier.phase!=self.phase
                || barrier.barrier_id.is_empty() || !valid_hash(&barrier.hash) || barrier.flow!="source"
                || barrier.status!="released" || barrier.waiters!=0 || barrier.cancellation_reason.is_some()
                || reached.event_type!="readBarrierReached" || reached.scope_id!=barrier.scope_id || reached.generation!=barrier.generation
                || reached.barrier_id!=barrier.barrier_id || reached.hash!=barrier.hash || reached.flow!=barrier.flow
                || reached.phase!=barrier.phase || !matches!(reached.read_kind.as_str(),"stdRead"|"asyncRead")
                || !self.objects.iter().any(|object|object.hash==barrier.hash && object.flow==barrier.flow
                    && object.placement==reached.placement && object.placement!="inline"
                    && object.purposes.iter().any(|p|p=="Asset") && object.work.read_bytes>0) {
                return Err("source actual physical read barrier identity or completion invalid".into());
            }
            if decimal_nanos(barrier.released_elapsed_nanos.as_deref().ok_or("source read barrier release clock absent")?)?
                <decimal_nanos(&reached.elapsed_nanos)? {return Err("source barrier release precedes actual reader".into());}
        }
        Ok(())
    }
    pub fn asset_work(&self)->Work {self.purposes.get("Asset").cloned().unwrap_or_default()}
}

#[derive(Clone,Debug,Deserialize,Serialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
pub struct EndReceipt {pub scope_id:String,pub observation:Snapshot}
#[derive(Clone,Debug,Deserialize,Serialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
pub struct ShutdownReceipt {pub complete:bool,pub root_removed:bool,pub observation:Snapshot,pub child_exit:i32}
#[derive(Clone,Debug,Deserialize,Serialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
pub struct SettledSource {pub end:EndReceipt,pub shutdown:ShutdownReceipt}
impl SettledSource {
    pub fn validate(&self)->Result<(),String> {
        self.end.observation.validate()?;self.shutdown.observation.validate()?;
        if self.end.scope_id!=self.end.observation.scope_id || self.shutdown.observation.scope_id!=self.end.scope_id
            || self.shutdown.observation.generation!=self.end.observation.generation
            || self.shutdown.observation.phase!=self.end.observation.phase || !self.shutdown.complete
            || !self.shutdown.root_removed || self.shutdown.child_exit!=0
            || serde_json::to_value(&self.end.observation).map_err(|e|e.to_string())?
                !=serde_json::to_value(&self.shutdown.observation).map_err(|e|e.to_string())? {
            return Err("source final shutdown differs from complete End receipt".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn empty()->Snapshot {Snapshot {scope_id:"synthetic-scope".into(),generation:1,phase:"restore".into(),role_count:1,complete:true,
        objects:vec![],placements:BTreeMap::new(),purposes:BTreeMap::new(),flows:BTreeMap::new(),hash_domains:BTreeMap::new(),
        total:Work::default(),unknown_work:Work::default(),violations:BTreeMap::new(),pending_workers:0,pending_requests:0,
        pending_body_work:PendingBodyWork::default(),read_barrier:None}}
    #[test]
    #[ignore = "measurement harness self-test; benchmarks/lww-native/run.ps1 runs it"]
    fn complete_source_requires_final_shutdown_and_no_late_work() {
        let snapshot=empty();let mut receipt=SettledSource {end:EndReceipt {scope_id:snapshot.scope_id.clone(),observation:snapshot.clone()},
            shutdown:ShutdownReceipt {complete:true,root_removed:true,observation:snapshot,child_exit:0}};
        receipt.validate().unwrap();receipt.shutdown.child_exit=1;assert!(receipt.validate().is_err());
        receipt.shutdown.child_exit=0;receipt.shutdown.observation.role_count+=1;assert!(receipt.validate().is_err());
        receipt.shutdown.observation=receipt.end.observation.clone();receipt.shutdown.root_removed=false;assert!(receipt.validate().is_err());
        receipt.shutdown.root_removed=true;receipt.end.observation.pending_body_work.upload_sessions=1;assert!(receipt.validate().is_err());
    }
    #[test]
    #[ignore = "measurement harness self-test; benchmarks/lww-native/run.ps1 runs it"]
    fn actual_source_reads_and_hashes_conserve_exact_rows_ranges_and_roles() {
        let mut snapshot=empty();let work=Work {open_attempts:1,opens:1,read_calls:1,read_bytes:20,
            hash_calls:1,hash_bytes:20,hash_finalizations:1,..Default::default()};
        let hash_work=Work {hash_calls:1,hash_bytes:20,hash_finalizations:1,..Default::default()};
        snapshot.objects.push(ObjectWork {hash:"a".repeat(64),purposes:vec!["Asset".into(),"Control".into()],placement:"file".into(),
            flow:"source".into(),work:work.clone(),read_ranges:vec![ReadRange {start:3,bytes:20,calls:1}],
            selected_ranges:vec![ReadRange {start:0,bytes:30,calls:1}],hash_domains:BTreeMap::from([("source-file".into(),hash_work.clone())])});
        snapshot.total=work.clone();snapshot.placements.insert("file".into(),work.clone());snapshot.flows.insert("source".into(),work.clone());
        snapshot.purposes.insert("Asset".into(),work);snapshot.hash_domains.insert("source-file".into(),hash_work);
        snapshot.validate().unwrap();assert!(!snapshot.asset_work().asset_zero());
        let valid=snapshot.clone();snapshot.objects[0].read_ranges[0].bytes=21;assert!(snapshot.validate().is_err());
        snapshot=valid.clone();snapshot.objects[0].selected_ranges[0].start=u64::MAX;assert!(snapshot.validate().is_err());
        snapshot=valid.clone();snapshot.hash_domains.clear();assert!(snapshot.validate().is_err());
        snapshot=valid.clone();snapshot.purposes.clear();assert!(snapshot.validate().is_err());
        snapshot=valid.clone();snapshot.objects[0].purposes=vec!["Unknown".into()];assert!(snapshot.validate().is_err());
        snapshot=valid;snapshot.objects[0].work.hash_finalizations=0;assert!(snapshot.validate().is_err());
    }
    #[test]
    #[ignore = "measurement harness self-test; benchmarks/lww-native/run.ps1 runs it"]
    fn inline_extraction_and_file_eof_do_not_invent_range_calls() {
        let mut snapshot=empty();let work=Work {open_attempts:1,opens:1,read_calls:2,read_bytes:20,..Default::default()};
        snapshot.objects.push(ObjectWork {hash:"b".repeat(64),purposes:vec!["Asset".into()],placement:"inline".into(),flow:"source".into(),
            work:work.clone(),read_ranges:vec![],selected_ranges:vec![],hash_domains:BTreeMap::new()});
        snapshot.total=work.clone();snapshot.placements.insert("inline".into(),work.clone());snapshot.flows.insert("source".into(),work.clone());
        snapshot.purposes.insert("Asset".into(),work);snapshot.validate().unwrap();
        snapshot.objects[0].placement="file".into();snapshot.placements=BTreeMap::from([("file".into(),snapshot.total.clone())]);
        snapshot.objects[0].read_ranges.push(ReadRange {start:0,bytes:20,calls:1});snapshot.validate().unwrap();
        snapshot.objects[0].read_ranges[0].calls=3;assert!(snapshot.validate().is_err());
    }
    #[test]
    #[ignore = "measurement harness self-test; benchmarks/lww-native/run.ps1 runs it"]
    fn physical_read_barrier_requires_exact_reached_release_and_positive_asset_read() {
        let mut snapshot=empty();let work=Work {open_attempts:1,opens:1,read_calls:1,read_bytes:1,..Default::default()};
        snapshot.objects.push(ObjectWork {hash:"c".repeat(64),purposes:vec!["Asset".into()],placement:"file".into(),flow:"source".into(),
            work:work.clone(),read_ranges:vec![ReadRange {start:0,bytes:1,calls:1}],selected_ranges:vec![],hash_domains:BTreeMap::new()});
        snapshot.total=work.clone();snapshot.placements.insert("file".into(),work.clone());snapshot.flows.insert("source".into(),work.clone());
        snapshot.purposes.insert("Asset".into(),work);
        let reached=ReadBarrierReached {event_type:"readBarrierReached".into(),scope_id:snapshot.scope_id.clone(),generation:1,
            barrier_id:"synthetic-barrier".into(),hash:"c".repeat(64),flow:"source".into(),phase:snapshot.phase.clone(),
            placement:"file".into(),read_kind:"asyncRead".into(),offset:0,elapsed_nanos:"10".into()};
        snapshot.read_barrier=Some(ReadBarrier {scope_id:snapshot.scope_id.clone(),generation:1,barrier_id:"synthetic-barrier".into(),
            hash:"c".repeat(64),flow:"source".into(),status:"released".into(),phase:snapshot.phase.clone(),waiters:0,
            reached:Some(reached),released_elapsed_nanos:Some("20".into()),cancellation_reason:None});
        snapshot.validate().unwrap();let valid=snapshot.clone();
        for status in ["armed","reached","cancelled"] {snapshot=valid.clone();snapshot.read_barrier.as_mut().unwrap().status=status.into();assert!(snapshot.validate().is_err());}
        snapshot=valid.clone();snapshot.read_barrier.as_mut().unwrap().waiters=1;assert!(snapshot.validate().is_err());
        snapshot=valid.clone();snapshot.read_barrier.as_mut().unwrap().released_elapsed_nanos=Some("9".into());assert!(snapshot.validate().is_err());
        snapshot=valid.clone();snapshot.read_barrier.as_mut().unwrap().reached.as_mut().unwrap().hash="d".repeat(64);assert!(snapshot.validate().is_err());
        snapshot=valid;snapshot.read_barrier.as_mut().unwrap().released_elapsed_nanos=Some("020".into());assert!(snapshot.validate().is_err());
        let mut encoded=serde_json::to_value(empty()).unwrap();encoded.as_object_mut().unwrap().remove("readBarrier");
        assert!(serde_json::from_value::<Snapshot>(encoded).is_err());
    }
}
