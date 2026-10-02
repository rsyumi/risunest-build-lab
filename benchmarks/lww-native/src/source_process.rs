use super::source_receipt::{EndReceipt,SettledSource,ShutdownReceipt,Snapshot};
use serde_json::{json,Value};
use sha2::{Digest,Sha256};
use std::{collections::{BTreeMap,BTreeSet},io::{BufRead,BufReader,Read,Write},path::Path,
    process::{Child,ChildStdin,Command,Stdio},sync::mpsc,time::Duration};

const PREFIX:&[u8]=b"RISUNEST_SYNC_SOURCE ";
const REPLY_BYTES:usize=256*1024*1024;
const REPLY_WAIT:Duration=Duration::from_secs(60);

pub struct PrivateRegistration {pub uri:String}
pub struct SourceProcess {
    child:Child,input:Option<ChildStdin>,replies:mpsc::Receiver<Result<Value,String>>,
    pub ready:Value,pub raw_observations:Vec<Value>,request_sequence:u64,active:Option<(String,u64,String)>,ended:Option<EndReceipt>,
    events:Vec<Value>,failed:bool,stopped:bool,
    inventory:BTreeMap<String,BTreeSet<String>>,barrier:Option<(String,String)>,
}
fn hex_hash(value:&str)->bool {value.len()==64 && value.bytes().all(|b|b.is_ascii_digit() || (b'a'..=b'f').contains(&b))}
fn read_record(input:&mut impl BufRead,limit:usize)->Result<Option<Vec<u8>>,String> {
    let mut record=Vec::new();
    loop {
        let bytes=input.fill_buf().map_err(|_|"source stdout read failed")?;
        if bytes.is_empty() {return if record.is_empty(){Ok(None)}else{Err("unterminated source stdout record".into())};}
        let count=bytes.iter().position(|&b|b==b'\n').map_or(bytes.len(),|n|n+1);
        if record.len().checked_add(count).is_none_or(|n|n>limit) {return Err("complete source reply exceeds explicit resource limit".into());}
        let done=bytes[count-1]==b'\n';record.extend_from_slice(&bytes[..count]);input.consume(count);
        if done {return Ok(Some(record));}
    }
}
fn verify_ready(ready:&Value,source:&str,binary:&str,run:&str)->Result<(),String> {
    if ready["type"]!="ready" || ready["sourceFingerprint"]!=source || ready["binarySha256"]!=binary
        || ready["runId"]!=run || ready["observerSchema"]!=1 || ready["maxCommandBytes"]!=67108864u64
        || ready["readBarrierHoldLimitMillis"]!=30000u64
        || ready["sourceComposition"]!="same-source-linked-object-delta-stream-transfer"
        || ready["structuralVerification"]!="normal-linked-recipe-field-move-and-frame-encode-parity"
        || ready["endpoint"].as_str().is_none_or(|s|!s.starts_with("http://127.0.0.1:"))
        || ready["rootId"].as_str().is_none_or(str::is_empty)
        || ready["sourceClosure"].as_array().is_none_or(Vec::is_empty) {
        return Err("source Ready identity or composition mismatch".into());
    }
    let mut paths=BTreeMap::new();let mut identities=BTreeSet::new();
    for entry in ready["sourceClosure"].as_array().unwrap() {
        let path=entry["path"].as_str().ok_or("source closure path missing")?;
        let role=entry["role"].as_str().ok_or("source closure role missing")?;
        let sha=entry["sha256"].as_str().ok_or("source closure SHA missing")?;
        if path.is_empty() || role.is_empty() || !identities.insert((path,role)) || !hex_hash(sha)
            || paths.insert(path,sha).is_some_and(|previous|previous!=sha) {return Err("source closure identity invalid".into());}
    }
    Ok(())
}
impl SourceProcess {
    pub fn start(executable:&Path,expected_source:&str,expected_binary:&str,run_id:&str)->Result<Self,String> {
        if !hex_hash(expected_source) || !hex_hash(expected_binary) || run_id.is_empty() {return Err("source launch identity absent".into());}
        let mut binary=std::fs::File::open(executable).map_err(|_|"frozen source executable unavailable")?;
        let mut digest=Sha256::new();let mut buffer=[0u8;64*1024];
        loop {let bytes=binary.read(&mut buffer).map_err(|_|"frozen source executable read failed")?;
            if bytes==0 {break;}digest.update(&buffer[..bytes]);}
        if hex::encode(digest.finalize())!=expected_binary {return Err("frozen source executable SHA mismatch".into());}
        let mut command=Command::new(executable);
        command.args(["--ignored","--exact","source_observer_harness::serve","--nocapture","--test-threads=1"])
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        #[cfg(windows)] {use std::os::windows::process::CommandExt;command.creation_flags(0x08000000);}
        let mut child=command.spawn().map_err(|_|"source libtest launch failed")?;
        let input=child.stdin.take().ok_or("source stdin unavailable")?;
        let stdout=child.stdout.take().ok_or("source stdout unavailable")?;
        let (send,replies)=mpsc::channel();
        std::thread::spawn(move || {
            let mut stdout=BufReader::new(stdout);
            loop {
                match read_record(&mut stdout,REPLY_BYTES) {
                    Ok(Some(line)) if line.starts_with(PREFIX)=>{
                        let reply=serde_json::from_slice(&line[PREFIX.len()..]).map_err(|_|"invalid source protocol reply".into());
                        let invalid=reply.is_err();if send.send(reply).is_err() || invalid {break;}
                    },
                    Ok(Some(_))=>{},
                    Ok(None)=>{let _=send.send(Err("source stdout ended before expected reply".into()));break;},
                    Err(reason)=>{let _=send.send(Err(reason));break;},
                }
            }
        });
        let mut process=Self {child,input:Some(input),replies,ready:Value::Null,raw_observations:vec![],request_sequence:0,active:None,ended:None,events:vec![],failed:false,stopped:false,
            inventory:BTreeMap::new(),barrier:None};
        let ready=process.exchange(json!({"op":"start","runId":run_id,"expectedSourceFingerprint":expected_source}),"ready")?;
        verify_ready(&ready,expected_source,expected_binary,run_id)?;process.ready=ready;Ok(process)
    }
    fn exchange(&mut self,mut command:Value,expected:&str)->Result<Value,String> {
        if self.failed || self.stopped {return Err("source process is invalid or stopped".into());}
        self.request_sequence=self.request_sequence.checked_add(1).ok_or("source request identity overflow")?;
        let id=format!("m-source-{}",self.request_sequence);command["requestId"]=id.clone().into();
        let mut bytes=serde_json::to_vec(&command).map_err(|_|"source command encoding failed")?;bytes.push(b'\n');
        let limit=self.ready["maxCommandBytes"].as_u64().unwrap_or(67108864);
        if bytes.len() as u64>limit {self.failed=true;return Err("whole source command exceeds Ready resource limit".into());}
        let input=self.input.as_mut().ok_or("source stdin is closed")?;
        if input.write_all(&bytes).and_then(|_|input.flush()).is_err() {self.failed=true;return Err("source command write failed".into());}
        loop {
            let reply=match self.replies.recv_timeout(REPLY_WAIT) {
                Ok(Ok(reply))=>reply,
                Ok(Err(reason))=>{self.failed=true;return Err(reason);},
                Err(_)=>{self.failed=true;return Err("source reply missing or timed out".into());},
            };
            if matches!(reply["type"].as_str(),Some("scopeEnded"|"stopped"|"error")) {self.raw_observations.push(reply.clone());}
            if reply["type"]=="readBarrierReached" {self.events.push(reply);continue;}
            if reply["type"]=="error" {self.failed=true;return Err(format!("source process explicitly rejected command: {}",
                reply["error"].as_str().unwrap_or("unreported structural error")));}
            if reply["requestId"]!=id || reply["type"]!=expected {self.failed=true;return Err("source reply identity mismatch".into());}
            return Ok(reply);
        }
    }
    pub fn register(&mut self,name:&str,registration_request_id:&str)->Result<PrivateRegistration,String> {
        if self.active.is_some() {return Err("registration during active scope".into());}
        let reply=self.exchange(json!({"op":"register","name":name,"registrationRequestId":registration_request_id}),"registration")?;
        Ok(PrivateRegistration {uri:reply["uri"].as_str().ok_or("private registration URI missing")?.to_owned()})
    }
    pub fn begin(&mut self,scope:&str,phase:&str,inventory:&BTreeMap<String,BTreeSet<String>>)->Result<Value,String> {
        if self.active.is_some() || scope.is_empty() || phase.is_empty() {return Err("invalid source scope transition".into());}
        let mut roles=vec![];
        for (hash,purposes) in inventory {
            if !hex_hash(hash) || purposes.is_empty() || purposes.iter().any(|p|p!="Control" && p!="Asset") {
                return Err("verified producer role inventory invalid".into());
            }
            roles.push(json!({"hash":hash,"purposes":purposes}));
        }
        let reply=self.exchange(json!({"op":"beginScope","scopeId":scope,"phase":phase,"roles":roles}),"scopeStarted")?;
        let generation=reply["generation"].as_u64().filter(|&n|n>0).ok_or("source generation absent")?;
        if reply["scopeId"]!=scope || reply["completeReset"]!=true {self.failed=true;return Err("source Begin reset not proven".into());}
        self.active=Some((scope.into(),generation,phase.into()));self.ended=None;self.inventory=inventory.clone();
        self.barrier=None;self.events.clear();Ok(reply)
    }
    pub fn arm_read_barrier(&mut self,barrier_id:&str,hash:&str)->Result<Value,String> {
        let (scope,generation,_)=self.active.as_ref().ok_or("source read barrier lacks active scope")?.clone();
        if self.barrier.is_some() || barrier_id.is_empty() || !self.inventory.get(hash).is_some_and(|roles|roles.contains("Asset")) {
            return Err("source read barrier lacks exact declared Asset identity".into());
        }
        let reply=self.exchange(json!({"op":"armReadBarrier","scopeId":scope,"generation":generation,
            "barrierId":barrier_id,"hash":hash,"flow":"source"}),"readBarrierArmed")?;
        self.check_barrier(&reply["barrier"],barrier_id,hash,"armed")?;
        self.barrier=Some((barrier_id.into(),hash.into()));Ok(reply)
    }
    fn check_barrier(&self,proof:&Value,id:&str,hash:&str,status:&str)->Result<(),String> {
        let (scope,generation,phase)=self.active.as_ref().ok_or("source read barrier scope absent")?;
        if proof["scopeId"]!=*scope || proof["generation"]!=*generation || proof["phase"]!=*phase
            || proof["barrierId"]!=id || proof["hash"]!=hash || proof["flow"]!="source" || proof["status"]!=status {
            return Err("source read barrier receipt identity differs".into());
        }
        Ok(())
    }
    pub fn wait_read_barrier(&mut self)->Result<Value,String> {
        let (id,hash)=self.barrier.clone().ok_or("source read barrier not armed")?;
        let event=if self.events.is_empty() {
            match self.replies.recv_timeout(REPLY_WAIT) {
                Ok(Ok(reply)) if reply["type"]=="readBarrierReached"=>reply,
                _=>{self.failed=true;return Err("actual source physical read barrier was not observed".into());},
            }
        } else {self.events.remove(0)};
        let (scope,generation,phase)=self.active.as_ref().ok_or("source barrier scope absent")?;
        if event["scopeId"]!=*scope || event["generation"]!=*generation || event["phase"]!=*phase
            || event["barrierId"]!=id || event["hash"]!=hash || event["flow"]!="source"
            || !matches!(event["readKind"].as_str(),Some("stdRead"|"asyncRead"))
            || event["placement"].as_str().is_none_or(|p|p.is_empty() || p=="inline")
            || event["offset"].as_u64().is_none() || event["elapsedNanos"].as_str().is_none_or(|s|
                s.parse::<u128>().ok().is_none_or(|n|n.to_string()!=s)) {
            self.failed=true;return Err("actual source read event identity or clock differs".into());
        }
        self.raw_observations.push(event.clone());Ok(event)
    }
    pub fn release_read_barrier(&mut self)->Result<Value,String> {
        let (id,hash)=self.barrier.clone().ok_or("source read barrier not armed")?;
        let (scope,generation,_)=self.active.as_ref().ok_or("source read barrier scope absent")?.clone();
        let reply=self.exchange(json!({"op":"releaseReadBarrier","scopeId":scope,"generation":generation,
            "barrierId":id,"hash":hash,"flow":"source"}),"readBarrierReleased")?;
        self.check_barrier(&reply["barrier"],&id,&hash,"released")?;Ok(reply)
    }
    pub fn end(&mut self)->Result<EndReceipt,String> {
        let (scope,generation,phase)=self.active.as_ref().ok_or("no active source scope")?.clone();
        let reply=self.exchange(json!({"op":"endScope","scopeId":scope}),"scopeEnded")?;
        if reply["sourceFingerprint"]!=self.ready["sourceFingerprint"] || reply["binarySha256"]!=self.ready["binarySha256"] {
            self.failed=true;return Err("source End identity changed".into());
        }
        let observation:Snapshot=serde_json::from_value(reply["observation"].clone()).map_err(|_|"unconsumed source observation schema")?;
        if observation.scope_id!=scope || observation.generation!=generation || observation.phase!=phase {
            self.failed=true;return Err("source End scope identity changed".into());
        }
        if observation.role_count!=self.inventory.len() as u64 || observation.objects.iter().any(|object|
            self.inventory.get(&object.hash).is_none_or(|roles|roles.iter().cloned().collect::<Vec<_>>()!=object.purposes)) {
            self.failed=true;return Err("source actual object roles differ from entire producer declaration".into());
        }
        observation.validate()?;let receipt=EndReceipt {scope_id:scope,observation};
        self.active=None;self.ended=Some(receipt.clone());Ok(receipt)
    }
    pub fn shutdown(&mut self)->Result<SettledSource,String> {
        if self.active.is_some() {return Err("source Shutdown before completed End".into());}
        let end=self.ended.clone().ok_or("source Shutdown lacks End receipt")?;
        let reply=self.exchange(json!({"op":"shutdown"}),"stopped")?;
        let observation:Snapshot=serde_json::from_value(reply["observation"].clone()).map_err(|_|"unconsumed source final schema")?;
        let waiting=std::time::Instant::now();
        let exit=loop {
            if let Some(status)=self.child.try_wait().map_err(|_|"source child exit unavailable")? {
                break status.code().ok_or("source child terminated without exit code")?;
            }
            if waiting.elapsed()>=REPLY_WAIT {self.failed=true;return Err("source child did not exit after Shutdown".into());}
            std::thread::sleep(Duration::from_millis(10));
        };
        self.stopped=true;
        let receipt=SettledSource {end,shutdown:ShutdownReceipt {complete:reply["complete"].as_bool().unwrap_or(false),
            root_removed:reply["rootRemoved"].as_bool().unwrap_or(false),observation,child_exit:exit}};
        receipt.validate()?;Ok(receipt)
    }
}
impl Drop for SourceProcess {
    fn drop(&mut self) {
        if !self.stopped {
            // EOF lets the private source drain readers and remove its synthetic root on failure.
            drop(self.input.take());
            let started=std::time::Instant::now();
            while started.elapsed()<REPLY_WAIT {
                match self.child.try_wait() {Ok(Some(_))=>return,Err(_)=>break,Ok(None)=>{}}
                std::thread::sleep(Duration::from_millis(10));
            }
            let _=self.child.kill();let _=self.child.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn whole_protocol_records_never_truncate_or_recover_from_partial_input() {
        assert_eq!(read_record(&mut &b"one\nsecond\n"[..],4).unwrap().unwrap(),b"one\n");
        assert!(read_record(&mut &b"oversized\n"[..],4).is_err());
        assert!(read_record(&mut &b"partial"[..],20).is_err());
        assert!(read_record(&mut &b""[..],20).unwrap().is_none());
    }
    #[test]
    fn compiled_closure_preserves_same_path_role_pairs_and_rejects_hash_disagreement() {
        let source="a".repeat(64);let binary="b".repeat(64);
        let mut ready=json!({"type":"ready","runId":"synthetic","sourceFingerprint":source,"binarySha256":binary,
            "observerSchema":1,"maxCommandBytes":67108864,"readBarrierHoldLimitMillis":30000,"endpoint":"http://127.0.0.1:1234","rootId":"synthetic-root",
            "sourceComposition":"same-source-linked-object-delta-stream-transfer",
            "structuralVerification":"normal-linked-recipe-field-move-and-frame-encode-parity",
            "sourceClosure":[{"path":"shared.rs","role":"normal","sha256":"c".repeat(64)},
                {"path":"shared.rs","role":"linked","sha256":"c".repeat(64)}]});
        verify_ready(&ready,&source,&binary,"synthetic").unwrap();
        let valid=ready.clone();ready["sourceClosure"][1]["role"]="normal".into();assert!(verify_ready(&ready,&source,&binary,"synthetic").is_err());
        ready=valid;ready["sourceClosure"][1]["sha256"]="d".repeat(64).into();assert!(verify_ready(&ready,&source,&binary,"synthetic").is_err());
    }
}
