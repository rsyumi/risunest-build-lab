use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct HashDomain {
    pub calls: u64,
    pub bytes: u64,
}

#[derive(Clone,Debug,Default,Serialize,Deserialize,PartialEq,Eq)]
pub struct PublicationDomain {
    pub attempts:u64,pub successes:u64,pub already_exists:u64,pub failures:u64,pub object_bytes:u64,
}
impl PublicationDomain {
    fn add(&mut self,other:&Self)->Result<(),String> {
        macro_rules! fields {($($field:ident),*)=>{$(self.$field=self.$field.checked_add(other.$field).ok_or("publication contribution overflow")?;)*}}
        fields!(attempts,successes,already_exists,failures,object_bytes);Ok(())
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BodyDomain {
    pub open_attempts: u64,
    pub opens: u64,
    pub failed_opens: u64,
    pub unknown_open_results: u64,
    pub read_operations: u64,
    pub read_bytes: u64,
    pub incomplete_reads: u64,
    pub escaped_handles: u64,
    pub escaped_paths: u64,
    pub outstanding_readers: u64,
    pub identity_metadata_open_attempts:u64,
    pub identity_metadata_opens:u64,
    pub identity_metadata_failed_opens:u64,
    pub verified_identity_metadata_opens:u64,
    pub staging_write_attempts:u64,pub staging_writes:u64,pub staging_failed_writes:u64,
    pub staging_requested_bytes:u64,pub staging_written_bytes:u64,pub incomplete_writes:u64,
    pub publication_attempts:u64,pub publications:u64,pub publication_already_exists:u64,
    pub publication_failures:u64,pub publication_object_bytes:u64,
    pub publication_kinds:BTreeMap<String,PublicationDomain>,
    pub body_sha:BTreeMap<String,HashDomain>,
}

impl BodyDomain {
    fn add(&mut self,work:&Self)->Result<(),String> {
        macro_rules! add {($($field:ident),*)=>{$(self.$field=self.$field.checked_add(work.$field)
            .ok_or("body contribution overflow")?;)*};}
        add!(open_attempts,opens,failed_opens,unknown_open_results,read_operations,read_bytes,
            incomplete_reads,escaped_handles,escaped_paths,outstanding_readers,
            identity_metadata_open_attempts,identity_metadata_opens,identity_metadata_failed_opens,
            verified_identity_metadata_opens,staging_write_attempts,staging_writes,staging_failed_writes,
            staging_requested_bytes,staging_written_bytes,incomplete_writes,publication_attempts,publications,
            publication_already_exists,publication_failures,publication_object_bytes);
        for (kind,work) in &work.publication_kinds {self.publication_kinds.entry(kind.clone()).or_default().add(work)?;}
        for (name,work) in &work.body_sha {
            let total=self.body_sha.entry(name.clone()).or_default();
            total.calls=total.calls.checked_add(work.calls).ok_or("body SHA call overflow")?;
            total.bytes=total.bytes.checked_add(work.bytes).ok_or("body SHA byte overflow")?;
        }
        Ok(())
    }
    fn complete(&self)->bool {
        self.failed_opens==0 && self.unknown_open_results==0 && self.incomplete_reads==0
            && self.escaped_handles==0 && self.escaped_paths==0 && self.outstanding_readers==0
            && self.identity_metadata_failed_opens==0 && self.open_attempts==self.opens
            && self.staging_failed_writes==0 && self.incomplete_writes==0 && self.staging_write_attempts==self.staging_writes
            && self.staging_requested_bytes==self.staging_written_bytes && self.publication_failures==0
            && self.publication_attempts==self.publications.checked_add(self.publication_already_exists).unwrap_or(u64::MAX)
            && self.publication_conservation().is_ok()
    }
    fn publication_conservation(&self)->Result<(),String> {
        let mut total=PublicationDomain::default();
        for (kind,work) in &self.publication_kinds {
            if !matches!(kind.as_str(),"hard-link"|"rename") || work.failures!=0
                || work.attempts!=work.successes.checked_add(work.already_exists).ok_or("publication result overflow")? {
                return Err("unrecognized or incomplete publication operation".into());
            }
            total.add(work)?;
        }
        if (total.attempts,total.successes,total.already_exists,total.failures,total.object_bytes)
            !=(self.publication_attempts,self.publications,self.publication_already_exists,self.publication_failures,self.publication_object_bytes) {
            return Err("publication kinds do not conserve actual summary".into());
        }
        if self.body_sha.keys().any(|name|!matches!(name.as_str(),"cas_stage"|"cas_import_adopt"|"cas_existing_verify")) {
            return Err("unknown native body SHA boundary".into());
        }
        Ok(())
    }
    fn identity_only(&self)->bool {
        self.complete() && self.read_operations==0 && self.read_bytes==0
            && self.open_attempts==self.opens && self.opens==self.identity_metadata_open_attempts
            && self.opens==self.identity_metadata_opens && self.opens==self.verified_identity_metadata_opens
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ObjectBodyDomain {
    pub purposes: Vec<String>,
    pub work: BodyDomain,
    pub owned_work: BodyDomain,
}
#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct NativeWorkerHashReceipt {
    pub thread:String,pub domains:BTreeMap<String,HashDomain>,pub incomplete:Vec<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct NativeObservation {
    pub units_visited: u64,
    pub hashes: BTreeMap<String, HashDomain>,
    pub incomplete: Vec<String>,
    pub requests: u64,
    pub uploaded_bytes: u64,
    pub downloaded_bytes: u64,
    pub messages_read: u64,
    pub message_bytes_read: u64,
    pub pages_written: u64,
    pub pages_reused: u64,
    pub asset_body_opens: Option<u64>,
    pub asset_body_bytes_read: Option<u64>,
    pub shared_head_reads: Option<u64>,
    pub body_scope_complete: Option<bool>,
    pub body_scope_thread: Option<String>,
    pub body_domains: BTreeMap<String, BodyDomain>,
    pub body_objects: BTreeMap<String, ObjectBodyDomain>,
    pub body_asset_work: BodyDomain,
    pub body_control_work: BodyDomain,
    pub body_owned_work: BodyDomain,
    pub body_unknown_work: BodyDomain,
    pub body_unattributed_managed: BodyDomain,
    pub body_unattributed_owned: BodyDomain,
    pub body_pending_owned_identities:u64,
    pub body_pending_hashes:u64,pub body_pending_worker_scopes:u64,pub body_worker_scopes_started:u64,
    pub body_worker_scopes_settled:u64,pub body_worker_threads:Vec<String>,
    pub native_caller_hashes:Option<BTreeMap<String,HashDomain>>,
    pub native_worker_hash_receipts:Vec<NativeWorkerHashReceipt>,
    pub body_scope_violations: u64,
    pub body_stat_requests: u64,
    pub body_batch_stat_requests: u64,
    pub body_presence_queries: u64,
    pub receive_intents:Option<super::invalid_diagnostics::ReceiveIntentEvidence>,
    pub commit_intents:Option<super::invalid_diagnostics::CommitIntentEvidence>,
}

impl NativeObservation {
    pub fn insert_domain(&mut self, owner: &str, domain: &str, calls: u64, bytes: u64) {
        let key = format!("{owner}/{domain}");
        assert!(self.hashes.insert(key, HashDomain { calls, bytes }).is_none(), "duplicate hash domain");
    }

    pub fn hash_totals(&self) -> Result<HashDomain, String> {
        if !self.incomplete.is_empty() {
            return Err(format!("incomplete SHA inputs: {:?}", self.incomplete));
        }
        self.hashes.values().try_fold(HashDomain::default(), |mut total, domain| {
            total.calls = total.calls.checked_add(domain.calls).ok_or("hash call overflow")?;
            total.bytes = total.bytes.checked_add(domain.bytes).ok_or("hash byte overflow")?;
            Ok(total)
        })
    }

    pub fn validate_routine_invariants(&self) -> Result<(), String> {
        self.hash_totals()?;
        if self.receive_intents.as_ref().is_some_and(|evidence|!evidence.complete) {
            return Err("incomplete exact receive-intent input provenance".into());
        }
        if self.commit_intents.as_ref().is_some_and(|evidence|!evidence.complete) {
            return Err("incomplete exact commit-intent input provenance".into());
        }
        if self.body_scope_complete != Some(true) || self.body_scope_violations != 0
            || self.body_unknown_work != BodyDomain::default()
            || self.body_unattributed_managed != BodyDomain::default()
            || self.body_unattributed_owned != BodyDomain::default()
            || self.body_pending_owned_identities != 0
            || self.body_pending_hashes!=0 || self.body_pending_worker_scopes!=0
            || self.body_worker_scopes_started!=self.body_worker_scopes_settled
            || self.body_worker_scopes_started!=0 || !self.body_worker_threads.is_empty()
            || !self.native_worker_hash_receipts.is_empty()
            || self.body_objects.values().any(|object| object.purposes.is_empty()
                && (object.work != BodyDomain::default() || object.owned_work != BodyDomain::default()))
            || self.body_domains.values().chain(self.body_objects.values().map(|object| &object.work))
                .chain(self.body_objects.values().map(|object| &object.owned_work)).any(|domain|
            !domain.complete()) {
            return Err("incomplete or unobserved CAS body scope".into());
        }
        self.validate_body_conservation(true)?;
        if self.body_asset_work!=BodyDomain::default() {return Err("routine Asset read/write/publication/SHA work is nonzero".into());}
        let mut body_sha=BodyDomain::default();
        for domain in self.body_domains.values() {body_sha.add(domain)?;}
        for (name,work) in body_sha.body_sha {
            let actual=self.hashes.get(&format!("native/{name}")).cloned().unwrap_or_default();
            if work.calls>actual.calls || work.bytes>actual.bytes {return Err("per-object body SHA is not an actual native SHA subset".into());}
        }
        if self.asset_body_opens != Some(self.body_asset_work.open_attempts)
            || self.asset_body_bytes_read != Some(self.body_asset_work.read_bytes) {
            return Err("inconsistent asset-purpose body totals".into());
        }
        for (domain, work) in &self.hashes {
            if (domain.contains("binding") || domain.contains("catalog_proof") || domain.contains("generation_digest"))
                && (work.calls != 0 || work.bytes != 0)
            {
                return Err(format!("routine binding proof: {domain}"));
            }
        }
        for (name, value) in [("asset body opens", self.asset_body_opens),
            ("asset body bytes", self.asset_body_bytes_read), ("shared head reads", self.shared_head_reads)] {
            match value {
                Some(0) => {},
                Some(_) => return Err(format!("nonzero {name}")),
                None => return Err(format!("unobserved {name}")),
            }
        }
        Ok(())
    }

    /// Costly operations allow declared Asset work, but cannot omit worker-native SHA receipts.
    pub fn validate_costly_native_coverage(&self)->Result<(),String> {
        self.hash_totals()?;
        if self.body_scope_complete!=Some(true) || self.body_scope_violations!=0
            || self.body_pending_owned_identities!=0 || self.body_pending_hashes!=0 || self.body_pending_worker_scopes!=0
            || self.body_worker_scopes_started!=self.body_worker_scopes_settled
            || self.body_unknown_work!=BodyDomain::default() || self.body_unattributed_managed!=BodyDomain::default()
            || self.body_unattributed_owned!=BodyDomain::default()
            || self.body_domains.values().chain(self.body_objects.values().flat_map(|object|[&object.work,&object.owned_work]))
                .any(|work|!work.complete()) {return Err("costly native body scope incomplete".into());}
        self.validate_body_conservation(false)?;
        let caller=self.native_caller_hashes.as_ref().ok_or("native caller SHA receipt unobserved")?;
        let mut expected=caller.clone();let mut workers=std::collections::BTreeSet::new();
        for worker in &self.native_worker_hash_receipts {
            if worker.thread.is_empty() || !worker.incomplete.is_empty() {return Err("native worker SHA receipt incomplete".into());}
            workers.insert(worker.thread.clone());
            for (name,work) in &worker.domains {
                let total=expected.entry(name.clone()).or_default();
                total.calls=total.calls.checked_add(work.calls).ok_or("worker SHA call overflow")?;
                total.bytes=total.bytes.checked_add(work.bytes).ok_or("worker SHA byte overflow")?;
            }
        }
        if self.native_worker_hash_receipts.len() as u64!=self.body_worker_scopes_settled
            || workers.into_iter().collect::<Vec<_>>()!=self.body_worker_threads
            || self.body_worker_threads.windows(2).any(|p|p[0]>=p[1]) {
            return Err("native worker SHA receipts do not cover actual entered body scopes".into());
        }
        let actual=self.hashes.iter().filter_map(|(name,work)|name.strip_prefix("native/").map(|name|(name.to_owned(),work.clone())))
            .collect::<BTreeMap<_,_>>();
        if expected!=actual {return Err("native caller plus worker SHA receipts do not conserve native totals".into());}
        self.validate_body_sha_subset()
    }
    fn validate_body_sha_subset(&self)->Result<(),String> {
        let mut total=BodyDomain::default();for work in self.body_domains.values() {total.add(work)?;}
        for (name,work) in total.body_sha {
            let actual=self.hashes.get(&format!("native/{name}")).cloned().unwrap_or_default();
            if work.calls>actual.calls || work.bytes>actual.bytes {return Err("per-object body SHA exceeds full native SHA receipts".into());}
        }Ok(())
    }
    pub fn attach_worker_hash_receipt(&mut self,receipt:NativeWorkerHashReceipt)->Result<(),String> {
        for (name,work) in &receipt.domains {
            let total=self.hashes.entry(format!("native/{name}")).or_default();
            total.calls=total.calls.checked_add(work.calls).ok_or("native worker SHA call overflow")?;
            total.bytes=total.bytes.checked_add(work.bytes).ok_or("native worker SHA byte overflow")?;
        }
        for incomplete in &receipt.incomplete {self.incomplete.push(format!("native-worker/{}:{incomplete}",receipt.thread));}
        self.native_worker_hash_receipts.push(receipt);Ok(())
    }
    fn validate_body_conservation(&self,routine:bool)->Result<(),String> {
        let mut managed=BodyDomain::default();let mut owned=BodyDomain::default();
        let mut control=BodyDomain::default();let mut asset=BodyDomain::default();
        let mut unknown=BodyDomain::default();
        for (hash,object) in &self.body_objects {
            if hash.len()!=64 || !hash.bytes().all(|v|v.is_ascii_hexdigit())
                || object.purposes.iter().any(|v|v!="Control" && v!="Asset")
                || object.purposes.iter().collect::<std::collections::BTreeSet<_>>().len()!=object.purposes.len() {
                return Err("invalid actual body identity or purpose inventory".into());
            }
            managed.add(&object.work)?;owned.add(&object.owned_work)?;
            let exclusive_control=object.purposes.len()==1 && object.purposes[0]=="Control";
            if routine && object.owned_work!=BodyDomain::default()
                && (!exclusive_control || !object.owned_work.identity_only()) {
                return Err("owned opens lack exclusive Control identity metadata proof".into());
            }
            if routine && exclusive_control {
                let staged=object.owned_work.body_sha.get("cas_stage").cloned().unwrap_or_default();
                if staged.bytes!=object.owned_work.staging_written_bytes
                    || (staged.calls==0)!=(object.owned_work.staging_writes==0)
                    || staged.calls>object.owned_work.staging_writes
                    || object.work.staging_write_attempts!=0
                    || object.work.body_sha.get("cas_stage").is_some_and(|work|work!=&HashDomain::default())
                    || object.work.body_sha.get("cas_import_adopt").is_some_and(|work|work!=&HashDomain::default())
                    || object.owned_work.body_sha.get("cas_import_adopt").is_some_and(|work|work!=&HashDomain::default()) {
                    return Err("routine Control staging does not conserve actual validated stage SHA inputs".into());
                }
            }
            let classified=if object.purposes.iter().any(|v|v=="Asset") {&mut asset}
                else if exclusive_control {&mut control} else {&mut unknown};
            classified.add(&object.work)?;classified.add(&object.owned_work)?;
        }
        for (name,work) in &self.body_domains {
            if name!="managed" && name!="owned" && work!=&BodyDomain::default() {
                return Err("unclassified body domain contribution".into());
            }
        }
        if managed!=self.body_domains.get("managed").cloned().unwrap_or_default()
            || owned!=self.body_domains.get("owned").cloned().unwrap_or_default()
            || owned!=self.body_owned_work || control!=self.body_control_work
            || asset!=self.body_asset_work || unknown!=self.body_unknown_work {
            return Err("per-object, domain and purpose body work does not conserve".into());
        }
        Ok(())
    }
}

#[derive(Debug, Serialize)]
pub struct InputAttribution {
    pub input_index:usize,
    pub baseline_bytes:u64,
    pub trial_bytes:u64,
    pub baseline_leaf_bytes:u64,
    pub trial_leaf_bytes:u64,
    pub attributed_growth:u64,
    pub matching_nodes:u64,
    pub unchanged_protected_bytes:u64,
}

#[derive(Debug, Serialize)]
pub struct DomainAttribution {
    pub domain:&'static str,
    pub leaf:&'static str,
    pub calls:u64,
    pub baseline_bytes:u64,
    pub trial_bytes:u64,
    pub attributed_growth:u64,
    pub inputs:Vec<InputAttribution>,
}

#[derive(Debug, Serialize)]
pub struct FrozenGrowthProof {
    pub aggregate_control_growth:u64,
    pub domains:Vec<DomainAttribution>,
    pub baseline_control_body_work:BodyDomain,
    pub trial_control_body_work:BodyDomain,
}

fn verify_input_collection(evidence:&super::invalid_diagnostics::ReceiveIntentEvidence,
    actual:&HashDomain) -> Result<(),String> {
    if !evidence.complete || !evidence.errors.is_empty()
        || evidence.observed_calls!=actual.calls || evidence.captured_calls!=actual.calls
        || evidence.inputs.len() as u64!=actual.calls
        || evidence.observed_bytes!=actual.bytes || evidence.captured_bytes!=actual.bytes {
        return Err("incomplete or inconsistent exact SHA input collection".into());
    }
    let mut bytes=0u64;
    for input in &evidence.inputs {
        bytes=bytes.checked_add(input.input_bytes).ok_or("input byte overflow")?;
        let own=input.nodes.values().try_fold(0u64,|total,node|total.checked_add(node.own_bytes).ok_or("partition overflow"))?;
        if own!=input.input_bytes || input.nodes.get("").map(|node|node.encoded_bytes)!=Some(input.input_bytes) {
            return Err("exact SHA input partition mismatch".into());
        }
        for (path,node) in &input.nodes {
            let children=input.nodes.iter().filter(|(child,_)| !child.is_empty()
                && child.rsplit_once('/').is_some_and(|(parent,_)|parent==path.as_str())).map(|(_,node)|node).collect::<Vec<_>>();
            let child_bytes=children.iter().try_fold(0u64,|sum,child|sum.checked_add(child.encoded_bytes).ok_or("child byte overflow"))?;
            if children.len() as u64!=node.children || node.own_bytes.checked_add(child_bytes)!=Some(node.encoded_bytes)
                || (!matches!(node.kind.as_str(),"array"|"object") && node.children!=0)
                || !matches!(node.kind.as_str(),"array"|"object"|"string"|"number"|"null"|"boolean") {
                return Err("invalid exact SHA node partition".into());
            }
            if !path.is_empty() && !input.nodes.contains_key(path.rsplit_once('/').ok_or("invalid node path")?.0) {
                return Err("orphan SHA input node".into());
            }
        }
    }
    if bytes!=actual.bytes {return Err("exact input lengths do not conserve SHA bytes".into());}
    Ok(())
}

fn prove_domain(a:&NativeObservation,b:&NativeObservation,domain:&'static str,leaf:&'static str,kind:&str,
    left:Option<&super::invalid_diagnostics::ReceiveIntentEvidence>,
    right:Option<&super::invalid_diagnostics::ReceiveIntentEvidence>) -> Result<DomainAttribution,String> {
    let smaller=a.hashes.get(domain).cloned().unwrap_or_default();
    let larger=b.hashes.get(domain).cloned().unwrap_or_default();
    let left=left.ok_or("missing exact baseline input collection")?;
    let right=right.ok_or("missing exact trial input collection")?;
    verify_input_collection(left,&smaller)?;
    verify_input_collection(right,&larger)?;
    if smaller.calls!=larger.calls {return Err("approved SHA domain input count changed".into());}
    let mut inputs=Vec::new();
    let mut growth=0u64;
    for (index,(a,b)) in left.inputs.iter().zip(&right.inputs).enumerate() {
        if !a.nodes.keys().eq(b.nodes.keys()) {return Err("SHA input structural node set changed".into());}
        let mut leaf_growth=0u64;
        let mut leaf_bytes=(0,0);
        for (path,left) in &a.nodes {
            let right=&b.nodes[path];
            if left.kind!=right.kind || left.children!=right.children {return Err("SHA input node shape changed".into());}
            if path.as_str()==leaf {
                if left.kind!=kind || left.children!=0 {return Err("approved control leaf type mismatch".into());}
                leaf_growth=right.own_bytes.checked_sub(left.own_bytes).ok_or("negative control growth cannot cancel work")?;
                leaf_bytes=(left.own_bytes,right.own_bytes);
            } else if left.own_bytes!=right.own_bytes {
                return Err(format!("protected or unknown SHA input contribution changed: {domain}:{path}"));
            }
        }
        if b.input_bytes.checked_sub(a.input_bytes)!=Some(leaf_growth) {return Err("SHA growth does not equal approved leaf growth".into());}
        growth=growth.checked_add(leaf_growth).ok_or("attribution overflow")?;
        inputs.push(InputAttribution {input_index:index,baseline_bytes:a.input_bytes,trial_bytes:b.input_bytes,
            baseline_leaf_bytes:leaf_bytes.0,trial_leaf_bytes:leaf_bytes.1,attributed_growth:leaf_growth,
            matching_nodes:a.nodes.len() as u64,unchanged_protected_bytes:a.input_bytes-leaf_bytes.0});
    }
    if larger.bytes.checked_sub(smaller.bytes)!=Some(growth) {return Err("domain SHA growth does not conserve approved attribution".into());}
    Ok(DomainAttribution {domain,leaf,calls:smaller.calls,baseline_bytes:smaller.bytes,
        trial_bytes:larger.bytes,attributed_growth:growth,inputs})
}

pub fn assert_frozen_growth(a:&NativeObservation,b:&NativeObservation) -> Result<FrozenGrowthProof,String> {
    a.validate_routine_invariants()?;
    b.validate_routine_invariants()?;
    if b.units_visited>a.units_visited || b.requests>a.requests {return Err("unit/request growth".into());}
    let mut proof=FrozenGrowthProof {aggregate_control_growth:0,domains:Vec::new(),
        baseline_control_body_work:a.body_control_work.clone(),trial_control_body_work:b.body_control_work.clone()};
    macro_rules! bounded {($($field:ident),*)=>{$(if b.body_control_work.$field>a.body_control_work.$field {
        return Err(format!("unrelated-library Control body growth: {}",stringify!($field)));})*};}
    bounded!(open_attempts,opens,read_operations,read_bytes,identity_metadata_open_attempts,identity_metadata_opens,
        verified_identity_metadata_opens,staging_write_attempts,staging_writes,staging_requested_bytes,staging_written_bytes,
        publication_attempts,publications,publication_already_exists,publication_object_bytes);
    for (kind,larger) in &b.body_control_work.publication_kinds {
        let smaller=a.body_control_work.publication_kinds.get(kind).cloned().unwrap_or_default();
        if larger.attempts>smaller.attempts || larger.successes>smaller.successes || larger.already_exists>smaller.already_exists
            || larger.object_bytes>smaller.object_bytes {return Err("unrelated-library Control publication growth".into());}
    }
    for (name,larger) in &b.body_control_work.body_sha {
        let smaller=a.body_control_work.body_sha.get(name).cloned().unwrap_or_default();
        if larger.calls>smaller.calls || larger.bytes>smaller.bytes {return Err("unrelated-library Control body SHA growth".into());}
    }
    for (domain,leaf,kind,left,right) in [
        ("native/native_receive_intent","/progress/cursor","string",a.receive_intents.as_ref(),b.receive_intents.as_ref()),
        ("native/native_intent","/commit/expectedRevision","number",a.commit_intents.as_ref(),b.commit_intents.as_ref())] {
        if a.hashes.contains_key(domain) || b.hashes.contains_key(domain) || left.is_some() || right.is_some() {
            let attribution=prove_domain(a,b,domain,leaf,kind,left,right)?;
            proof.aggregate_control_growth=proof.aggregate_control_growth.checked_add(attribution.attributed_growth).ok_or("growth overflow")?;
            proof.domains.push(attribution);
        }
    }
    for (key,larger) in &b.hashes {
        let smaller=a.hashes.get(key).cloned().unwrap_or_default();
        if larger.calls>smaller.calls {return Err(format!("hash call growth: {key}"));}
        if larger.bytes>smaller.bytes && !proof.domains.iter().any(|domain|domain.domain==key.as_str()) {
            return Err(format!("unattributed hash byte growth: {key}"));
        }
    }
    if proof.aggregate_control_growth>8192 {return Err("control metadata growth exceeds 8 KiB".into());}
    Ok(proof)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn costly_native_sha_needs_every_actual_worker_receipt_without_double_counting() {
        let mut sample=NativeObservation {body_scope_complete:Some(true),native_caller_hashes:Some(BTreeMap::new()),
            body_worker_scopes_started:1,body_worker_scopes_settled:1,body_worker_threads:vec!["ThreadId(7)".into()],..Default::default()};
        assert!(sample.validate_costly_native_coverage().is_err());
        sample.attach_worker_hash_receipt(NativeWorkerHashReceipt {thread:"ThreadId(7)".into(),
            domains:BTreeMap::from([("native_unit_envelope".into(),HashDomain {calls:1,bytes:100})]),incomplete:vec![]}).unwrap();
        sample.validate_costly_native_coverage().unwrap();assert_eq!(sample.hash_totals().unwrap().bytes,100);
        let valid=sample.clone();sample.hashes.get_mut("native/native_unit_envelope").unwrap().bytes=200;
        assert!(sample.validate_costly_native_coverage().is_err());
        sample=valid.clone();sample.native_worker_hash_receipts[0].thread="ThreadId(8)".into();assert!(sample.validate_costly_native_coverage().is_err());
        sample=valid.clone();sample.native_worker_hash_receipts[0].incomplete.push("partial input".into());assert!(sample.validate_costly_native_coverage().is_err());
        sample=valid.clone();sample.body_pending_worker_scopes=1;assert!(sample.validate_costly_native_coverage().is_err());
        sample=valid;sample.native_caller_hashes=None;assert!(sample.validate_costly_native_coverage().is_err());
    }
    #[test]
    fn control_stage_and_publication_are_distinct_conserved_sha_subset_evidence() {
        let owned=BodyDomain {open_attempts:2,opens:2,identity_metadata_open_attempts:2,identity_metadata_opens:2,
            verified_identity_metadata_opens:2,staging_write_attempts:2,staging_writes:2,staging_requested_bytes:7,staging_written_bytes:7,
            body_sha:BTreeMap::from([("cas_stage".into(),HashDomain {calls:1,bytes:7})]),..Default::default()};
        let managed=BodyDomain {publication_attempts:1,publications:1,publication_object_bytes:7,
            publication_kinds:BTreeMap::from([("hard-link".into(),PublicationDomain {attempts:1,successes:1,object_bytes:7,..Default::default()})]),
            ..Default::default()};
        let mut total=owned.clone();total.add(&managed).unwrap();
        let mut sample=NativeObservation {asset_body_opens:Some(0),asset_body_bytes_read:Some(0),shared_head_reads:Some(0),body_scope_complete:Some(true),
            body_control_work:total,body_owned_work:owned.clone(),body_domains:BTreeMap::from([("managed".into(),managed.clone()),("owned".into(),owned.clone())]),
            body_objects:BTreeMap::from([("ab".repeat(32),ObjectBodyDomain {purposes:vec!["Control".into()],work:managed,owned_work:owned})]),
            hashes:BTreeMap::from([("native/cas_stage".into(),HashDomain {calls:1,bytes:7})]),..Default::default()};
        sample.validate_routine_invariants().unwrap();assert_frozen_growth(&sample,&sample).unwrap();
        let valid=sample.clone();sample.hashes.get_mut("native/cas_stage").unwrap().bytes=6;assert!(sample.validate_routine_invariants().is_err());
        sample=valid.clone();sample.body_objects.values_mut().next().unwrap().owned_work.staging_written_bytes=8;assert!(sample.validate_routine_invariants().is_err());
        sample=valid.clone();sample.body_domains.get_mut("managed").unwrap().publication_kinds.clear();assert!(sample.validate_routine_invariants().is_err());
        sample=valid.clone();sample.body_worker_scopes_started=1;sample.body_worker_scopes_settled=1;assert!(sample.validate_routine_invariants().is_err());
        sample=valid.clone();let object=sample.body_objects.values_mut().next().unwrap();object.purposes=vec!["Asset".into(),"Control".into()];
        sample.body_asset_work=sample.body_control_work.clone();sample.body_control_work=BodyDomain::default();assert!(sample.validate_routine_invariants().is_err());
        sample=valid.clone();sample.body_objects.values_mut().next().unwrap().work.publication_kinds.get_mut("hard-link").unwrap().object_bytes=8;
        assert!(sample.validate_routine_invariants().is_err());
        let mut grown=valid.clone();grown.hashes.get_mut("native/cas_stage").unwrap().bytes=8;
        for work in [&mut grown.body_control_work,&mut grown.body_owned_work,grown.body_domains.get_mut("owned").unwrap(),
            &mut grown.body_objects.values_mut().next().unwrap().owned_work] {
            work.staging_requested_bytes=8;work.staging_written_bytes=8;work.body_sha.get_mut("cas_stage").unwrap().bytes=8;
        }
        grown.validate_routine_invariants().unwrap();assert!(assert_frozen_growth(&valid,&grown).is_err());
    }
    #[test]
    fn incomplete_and_unobserved_inputs_cannot_pass() {
        let mut sample = NativeObservation::default();
        assert!(sample.validate_routine_invariants().is_err());
        sample.asset_body_opens = Some(0);
        sample.asset_body_bytes_read = Some(0);
        sample.shared_head_reads = Some(0);
        sample.body_scope_complete = Some(true);
        assert!(sample.validate_routine_invariants().is_ok());
        sample.commit_intents=Some(Default::default());
        assert_eq!(sample.validate_routine_invariants().unwrap_err(),"incomplete exact commit-intent input provenance");
        sample.commit_intents=None;
        sample.incomplete.push("native/stream".into());
        assert!(sample.hash_totals().is_err());
    }
    #[test]
    fn role_coverage_and_reader_lifecycle_remain_fail_closed() {
        let mut sample = NativeObservation {asset_body_opens:Some(0), asset_body_bytes_read:Some(0),
            shared_head_reads:Some(0), body_scope_complete:Some(true), ..Default::default()};
        sample.body_control_work = BodyDomain {open_attempts:1, opens:1, read_operations:1,
            read_bytes:22, ..Default::default()};
        sample.body_domains.insert("managed".into(), sample.body_control_work.clone());
        sample.body_objects.insert("ab".repeat(32),ObjectBodyDomain {purposes:vec!["Control".into()],
            work:sample.body_control_work.clone(),..Default::default()});
        sample.validate_routine_invariants().unwrap();
        sample.body_unknown_work.open_attempts = 1;
        assert!(sample.validate_routine_invariants().is_err());
        sample.body_unknown_work = BodyDomain::default();
        sample.body_domains.get_mut("managed").unwrap().outstanding_readers = 1;
        assert!(sample.validate_routine_invariants().is_err());
        sample.body_domains.get_mut("managed").unwrap().outstanding_readers = 0;
        sample.body_scope_violations = 1;
        assert!(sample.validate_routine_invariants().is_err());
        sample.body_scope_violations = 0;
        sample.body_owned_work.read_bytes = 1;
        assert!(sample.validate_routine_invariants().is_err());
    }

    #[test]
    fn owned_control_identity_proof_is_exact_and_conserves_all_contributions() {
        let mut sample=NativeObservation {asset_body_opens:Some(0),asset_body_bytes_read:Some(0),
            shared_head_reads:Some(0),body_scope_complete:Some(true),..Default::default()};
        let hash="ab".repeat(32);
        let owned=BodyDomain {open_attempts:4,opens:4,identity_metadata_open_attempts:4,
            identity_metadata_opens:4,verified_identity_metadata_opens:4,..Default::default()};
        sample.body_objects.insert(hash.clone(),ObjectBodyDomain {purposes:vec!["Control".into()],
            owned_work:owned.clone(),..Default::default()});
        sample.body_owned_work=owned.clone();sample.body_control_work=owned.clone();
        sample.body_domains.insert("owned".into(),owned.clone());sample.validate_routine_invariants().unwrap();
        for (field,value) in [("identity_metadata_open_attempts",3),("identity_metadata_opens",3),
            ("verified_identity_metadata_opens",3),("verified_identity_metadata_opens",5),
            ("identity_metadata_failed_opens",1),("failed_opens",1),("unknown_open_results",1),
            ("read_operations",1),("read_bytes",1),("incomplete_reads",1),("escaped_handles",1),
            ("escaped_paths",1),("outstanding_readers",1)] {
            let mut changed=sample.clone();let mut body=serde_json::to_value(&owned).unwrap();body[field]=value.into();
            let body:BodyDomain=serde_json::from_value(body).unwrap();
            changed.body_objects.get_mut(&hash).unwrap().owned_work=body.clone();
            changed.body_owned_work=body.clone();changed.body_control_work=body.clone();
            changed.body_domains.insert("owned".into(),body);
            assert!(changed.validate_routine_invariants().is_err(),"conserved invalid owned field {field}");
        }
        for purposes in [vec!["Asset".into()],vec!["Control".into(),"Asset".into()],Vec::new()] {
            let mut changed=sample.clone();changed.body_objects.get_mut(&hash).unwrap().purposes=purposes.clone();
            changed.body_control_work=BodyDomain::default();
            if purposes.is_empty() {changed.body_unknown_work=owned.clone();}
            else {changed.body_asset_work=owned.clone();changed.asset_body_opens=Some(4);}
            assert!(changed.validate_routine_invariants().is_err());
        }
        let mut raw=sample.clone();let mut work=owned.clone();
        work.identity_metadata_open_attempts=0;work.identity_metadata_opens=0;work.verified_identity_metadata_opens=0;
        raw.body_objects.get_mut(&hash).unwrap().owned_work=work.clone();raw.body_owned_work=work.clone();
        raw.body_control_work=work.clone();raw.body_domains.insert("owned".into(),work);
        assert!(raw.validate_routine_invariants().is_err());
        for tamper in 0..7 {
            let mut changed=sample.clone();match tamper {
                0=>changed.body_owned_work.opens+=1,
                1=>changed.body_control_work.verified_identity_metadata_opens+=1,
                2=>changed.body_domains.get_mut("owned").unwrap().identity_metadata_open_attempts+=1,
                3=>{changed.body_objects.clear();},
                4=>changed.body_unattributed_owned.open_attempts=1,
                5=>changed.body_pending_owned_identities=1,
                _=>{changed.body_domains.insert("unrecognized".into(),owned.clone());},
            }
            assert!(changed.validate_routine_invariants().is_err(),"conservation tamper {tamper}");
        }
        let mut overflow=sample.clone();let maximal=BodyDomain {open_attempts:u64::MAX,opens:u64::MAX,
            identity_metadata_open_attempts:u64::MAX,identity_metadata_opens:u64::MAX,
            verified_identity_metadata_opens:u64::MAX,..Default::default()};
        overflow.body_objects.get_mut(&hash).unwrap().owned_work=maximal;
        overflow.body_objects.insert("cd".repeat(32),sample.body_objects[&hash].clone());
        assert_eq!(overflow.validate_routine_invariants().unwrap_err(),"body contribution overflow");
    }

    #[test]
    fn growth_requires_precise_control_attribution_and_never_more_calls() {
        let mut a = NativeObservation { asset_body_opens:Some(0), asset_body_bytes_read:Some(0),
            shared_head_reads:Some(0), body_scope_complete:Some(true), ..Default::default() };
        a.insert_domain("C", "intent", 1, 100);
        let mut b = a.clone();
        b.hashes.get_mut("C/intent").unwrap().bytes += 10;
        assert!(assert_frozen_growth(&a, &b).is_err());
        b.hashes.get_mut("C/intent").unwrap().calls += 1;
        assert!(assert_frozen_growth(&a, &b).is_err());
    }

    fn observed(domain:&str,input:&[u8]) -> NativeObservation {
        let mut sample=NativeObservation {asset_body_opens:Some(0),asset_body_bytes_read:Some(0),
            shared_head_reads:Some(0),body_scope_complete:Some(true),..Default::default()};
        sample.hashes.insert(domain.into(),HashDomain {calls:1,bytes:input.len() as u64});
        let mut evidence=super::super::invalid_diagnostics::ReceiveIntentEvidence {
            observed_calls:1,captured_calls:1,observed_bytes:input.len() as u64,captured_bytes:input.len() as u64,
            inputs:vec![super::super::invalid_diagnostics::exact_json_structure(input,input).unwrap()],..Default::default()};
        evidence.finish();
        if domain=="native/native_intent" {sample.commit_intents=Some(evidence);}
        else {sample.receive_intents=Some(evidence);}
        sample
    }

    fn proof_pairs() -> Vec<(NativeObservation,NativeObservation)> {
        vec![(observed("native/native_receive_intent",br#"{"progress":{"cursor":"9"},"changes":[{"value":"same"}]}"#),
            observed("native/native_receive_intent",br#"{"progress":{"cursor":"10"},"changes":[{"value":"same"}]}"#)),
            (observed("native/native_intent",br#"{"commit":{"expectedRevision":9,"value":"same"},"aliases":[]}"#),
            observed("native/native_intent",br#"{"commit":{"expectedRevision":10,"value":"same"},"aliases":[]}"#))]
    }

    fn collection(sample:&mut NativeObservation) -> &mut super::super::invalid_diagnostics::ReceiveIntentEvidence {
        if sample.commit_intents.is_some() {sample.commit_intents.as_mut().unwrap()}
        else {sample.receive_intents.as_mut().unwrap()}
    }

    #[test]
    fn approved_exact_leaves_produce_conserved_per_input_attribution() {
        for (a,b) in proof_pairs() {
            let proof=assert_frozen_growth(&a,&b).unwrap();
            assert_eq!(proof.aggregate_control_growth,1);
            assert_eq!(proof.domains[0].inputs.len(),1);
            assert_eq!(proof.domains[0].inputs[0].attributed_growth,1);
            assert_eq!(proof.domains[0].trial_bytes-proof.domains[0].baseline_bytes,1);
        }
    }

    #[test]
    fn incomplete_missing_unpaired_and_extra_call_inputs_cannot_bypass_proof() {
        for (a,b) in proof_pairs() {
            let mut invalid=b.clone();collection(&mut invalid).complete=false;
            assert!(assert_frozen_growth(&a,&invalid).is_err());
            let mut invalid=b.clone();collection(&mut invalid).errors.push("roundtrip failed".into());
            assert!(assert_frozen_growth(&a,&invalid).is_err());
            let mut invalid=b.clone();invalid.receive_intents=None;invalid.commit_intents=None;
            assert!(assert_frozen_growth(&a,&invalid).is_err());
            let mut invalid=b.clone();collection(&mut invalid).inputs.clear();
            assert!(assert_frozen_growth(&a,&invalid).is_err());
            let mut invalid=b.clone();invalid.hashes.values_mut().next().unwrap().calls+=1;
            assert!(assert_frozen_growth(&a,&invalid).is_err());
            let mut invalid=b.clone();
            for work in invalid.hashes.values_mut() {work.calls*=2;work.bytes*=2;}
            let evidence=collection(&mut invalid);
            evidence.inputs.push(evidence.inputs[0].clone());
            evidence.observed_calls*=2;evidence.captured_calls*=2;
            evidence.observed_bytes*=2;evidence.captured_bytes*=2;
            assert!(assert_frozen_growth(&a,&invalid).is_err());
            let mut invalid=b.clone();collection(&mut invalid).captured_bytes+=1;
            assert!(assert_frozen_growth(&a,&invalid).is_err());
        }
    }

    #[test]
    fn node_set_kind_children_and_partition_tampering_cannot_bypass_proof() {
        for (a,b) in proof_pairs() {
            let mut invalid=b.clone();collection(&mut invalid).inputs[0].nodes.remove("");
            assert!(assert_frozen_growth(&a,&invalid).is_err());
            let mut invalid=b.clone();collection(&mut invalid).inputs[0].nodes.get_mut("").unwrap().kind="array".into();
            assert!(assert_frozen_growth(&a,&invalid).is_err());
            let mut invalid=b.clone();collection(&mut invalid).inputs[0].nodes.get_mut("").unwrap().children+=1;
            assert!(assert_frozen_growth(&a,&invalid).is_err());
            let mut invalid=b.clone();collection(&mut invalid).inputs[0].nodes.get_mut("").unwrap().own_bytes+=1;
            assert!(assert_frozen_growth(&a,&invalid).is_err());
            let mut invalid=b.clone();collection(&mut invalid).inputs[0].input_bytes+=1;
            assert!(assert_frozen_growth(&a,&invalid).is_err());
        }
    }

    #[test]
    fn protected_payload_negative_cancellation_unknown_fields_and_domains_reject() {
        for (domain,a,b) in [
            ("native/native_receive_intent",br#"{"progress":{"cursor":"9"},"changes":[{"value":"same"}]}"#.as_slice(),
                br#"{"progress":{"cursor":"10"},"changes":[{"value":"sam"}]}"#.as_slice()),
            ("native/native_intent",br#"{"commit":{"expectedRevision":9,"value":"same"},"aliases":[]}"#.as_slice(),
                br#"{"commit":{"expectedRevision":10,"value":"sam"},"aliases":[]}"#.as_slice())] {
            assert!(assert_frozen_growth(&observed(domain,a),&observed(domain,b)).is_err());
        }
        for (a,b) in proof_pairs() {
            assert!(assert_frozen_growth(&b,&a).is_err());
            let mut invalid=b.clone();invalid.insert_domain("other","unknown",1,1);
            assert!(assert_frozen_growth(&a,&invalid).is_err());
            let mut invalid=b.clone();invalid.requests+=1;
            assert!(assert_frozen_growth(&a,&invalid).is_err());
            let mut invalid=b.clone();invalid.units_visited+=1;
            assert!(assert_frozen_growth(&a,&invalid).is_err());
        }
        let a=observed("native/native_receive_intent",br#"{"progress":{"cursor":"9"}}"#);
        let b=observed("native/native_receive_intent",br#"{"progress":{"cursor":"10"},"unknown":"added"}"#);
        assert!(assert_frozen_growth(&a,&b).is_err());
        for (domain,left,right) in [
            ("native/native_receive_intent",br#"{"progress":{"cursor":"9"},"changes":[{"value":"same"}]}"#.as_slice(),
                br#"{"progress":{"cursor":"10"},"changes":[{"value":"longer"}]}"#.as_slice()),
            ("native/native_intent",br#"{"commit":{"expectedRevision":9,"key":"same"},"aliases":[]}"#.as_slice(),
                br#"{"commit":{"expectedRevision":10,"key":"same"},"aliases":[{"key":"asset"}]}"#.as_slice()),
            ("native/native_receive_intent",br#"{"progress":{"cursor":"9"},"changes":[{"key":"same"}]}"#.as_slice(),
                br#"{"progress":{"cursor":"10"},"changes":[{"renamed":"same"}]}"#.as_slice()),
            ("native/native_intent",br#"{"commit":{"expectedRevision":9}}"#.as_slice(),
                br#" {"commit":{"expectedRevision":10}}"#.as_slice())] {
            assert!(assert_frozen_growth(&observed(domain,left),&observed(domain,right)).is_err());
        }
    }

    #[test]
    fn original_8192_byte_cap_is_aggregate_across_both_precise_domains() {
        fn repeat(sample:&mut NativeObservation,count:u64) {
            for domain in sample.hashes.values_mut() {domain.calls*=count;domain.bytes*=count;}
            let evidence=collection(sample);
            evidence.inputs=vec![evidence.inputs[0].clone();count as usize];
            evidence.observed_calls*=count;evidence.captured_calls*=count;
            evidence.observed_bytes*=count;evidence.captured_bytes*=count;
        }
        let mut pairs=proof_pairs();
        let (mut a,mut b)=pairs.remove(0);let (mut commit_a,mut commit_b)=pairs.remove(0);
        repeat(&mut a,4096);repeat(&mut b,4096);repeat(&mut commit_a,4096);repeat(&mut commit_b,4096);
        a.hashes.extend(commit_a.hashes);a.commit_intents=commit_a.commit_intents;
        b.hashes.extend(commit_b.hashes);b.commit_intents=commit_b.commit_intents;
        assert_eq!(assert_frozen_growth(&a,&b).unwrap().aggregate_control_growth,8192);
        let (mut commit_a,mut commit_b)=proof_pairs().remove(1);
        repeat(&mut commit_a,4097);repeat(&mut commit_b,4097);
        a.hashes.extend(commit_a.hashes);a.commit_intents=commit_a.commit_intents;
        b.hashes.extend(commit_b.hashes);b.commit_intents=commit_b.commit_intents;
        assert_eq!(assert_frozen_growth(&a,&b).unwrap_err(),"control metadata growth exceeds 8 KiB");
    }
}
