//! Scaling ladder: one rung constructor and one phase-group runner, both ignored entry tests.
//! Rungs are built once and only ever read; every phase works on a fork whose CAS objects are
//! hard links into the rung. Each phase appends one JSON line as soon as it ends.
use super::*;
use std::{collections::{BTreeMap,BTreeSet},io::Write,path::PathBuf,sync::atomic::{AtomicBool,AtomicU64,Ordering},time::Duration};

const LINE_SCHEMA:&str="risunest.lww-ladder/v1";
const RUNG_SCHEMA:&str="risunest.lww-ladder-rung/v1";
const ROUTINE_CHARACTER:&str="synthetic-character-0";
const GC_GRACE_MS:i64=7*24*60*60*1_000;

trait Text<T> {
    fn text(self)->Result<T,String>;
    fn at(self,step:&str)->Result<T,String>;
}
impl<T,E:std::fmt::Debug> Text<T> for Result<T,E> {
    fn text(self)->Result<T,String> {self.map_err(|error|format!("{error:?}"))}
    fn at(self,step:&str)->Result<T,String> {self.map_err(|error|format!("{step}: {error:?}"))}
}

fn environment(name:&str)->Result<String,String> {std::env::var(name).map_err(|_|format!("{name} is required"))}
fn environment_u64(name:&str,default:Option<u64>)->Result<u64,String> {
    match std::env::var(name) {
        Ok(value)=>value.trim().replace('_',"").parse().map_err(|_|format!("{name} must be an unsigned integer")),
        Err(_)=>default.ok_or_else(||format!("{name} is required")),
    }
}
fn elapsed_ms(started:Instant)->f64 {started.elapsed().as_secs_f64()*1000.0}
fn epoch_ms()->i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|value|value.as_millis() as i64).unwrap_or(0)
}
fn panic_text(panic:Box<dyn std::any::Any+Send>)->String {
    panic.downcast_ref::<&str>().map(|text|text.to_string()).or_else(||panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(||"panic without a message".into())
}
fn median(values:&[f64])->Option<f64> {
    let mut sorted=values.to_vec();sorted.sort_by(|a,b|a.total_cmp(b));
    (!sorted.is_empty()).then(||sorted[sorted.len()/2])
}

#[derive(Clone,Copy,Default,Serialize)]
#[serde(rename_all="camelCase")]
struct Memory {working_set:Option<u64>,private_bytes:Option<u64>,process_peak_working_set:Option<u64>,process_peak_private_bytes:Option<u64>}

#[derive(Clone,Copy,Default)]
struct ProcessIo {read_operations:u64,write_operations:u64,read_bytes:u64,write_bytes:u64}

#[cfg(windows)]
mod os {
    #[repr(C)]
    #[derive(Default)]
    struct MemoryCounters {
        cb:u32,page_fault_count:u32,peak_working_set_size:usize,working_set_size:usize,
        quota_peak_paged_pool_usage:usize,quota_paged_pool_usage:usize,quota_peak_non_paged_pool_usage:usize,
        quota_non_paged_pool_usage:usize,pagefile_usage:usize,peak_pagefile_usage:usize,private_usage:usize,
    }
    #[repr(C)]
    #[derive(Default)]
    struct IoCounters {
        read_operation_count:u64,write_operation_count:u64,other_operation_count:u64,
        read_transfer_count:u64,write_transfer_count:u64,other_transfer_count:u64,
    }
    #[link(name="kernel32")]
    extern "system" {
        fn GetCurrentProcess()->*mut std::ffi::c_void;
        fn K32GetProcessMemoryInfo(process:*mut std::ffi::c_void,counters:*mut MemoryCounters,size:u32)->i32;
        fn GetProcessIoCounters(process:*mut std::ffi::c_void,counters:*mut IoCounters)->i32;
    }
    pub(super) fn memory()->super::Memory {
        let mut counters=MemoryCounters {cb:std::mem::size_of::<MemoryCounters>() as u32,..Default::default()};
        let ok=unsafe {K32GetProcessMemoryInfo(GetCurrentProcess(),&mut counters,counters.cb)};
        if ok==0 {return super::Memory::default();}
        super::Memory {working_set:Some(counters.working_set_size as u64),private_bytes:Some(counters.private_usage as u64),
            process_peak_working_set:Some(counters.peak_working_set_size as u64),process_peak_private_bytes:Some(counters.peak_pagefile_usage as u64)}
    }
    pub(super) fn io()->Option<super::ProcessIo> {
        let mut counters=IoCounters::default();
        let ok=unsafe {GetProcessIoCounters(GetCurrentProcess(),&mut counters)};
        (ok!=0).then_some(super::ProcessIo {read_operations:counters.read_operation_count,write_operations:counters.write_operation_count,
            read_bytes:counters.read_transfer_count,write_bytes:counters.write_transfer_count})
    }
}

#[cfg(target_os="linux")]
mod os {
    fn status_kib(name:&str)->Option<u64> {
        let status=std::fs::read_to_string("/proc/self/status").ok()?;
        let line=status.lines().find(|line|line.starts_with(name))?;
        line[name.len()..].trim().trim_end_matches("kB").trim().parse::<u64>().ok().map(|kib|kib*1024)
    }
    pub(super) fn memory()->super::Memory {
        super::Memory {working_set:status_kib("VmRSS:"),private_bytes:status_kib("RssAnon:"),
            process_peak_working_set:status_kib("VmHWM:"),process_peak_private_bytes:None}
    }
    pub(super) fn io()->Option<super::ProcessIo> {
        let io=std::fs::read_to_string("/proc/self/io").ok()?;
        let field=|name:&str|io.lines().find_map(|line|line.strip_prefix(name)).and_then(|value|value.trim().parse::<u64>().ok());
        Some(super::ProcessIo {read_operations:field("syscr:")?,write_operations:field("syscw:")?,
            read_bytes:field("read_bytes:")?,write_bytes:field("write_bytes:")?})
    }
}

#[cfg(not(any(windows,target_os="linux")))]
mod os {
    pub(super) fn memory()->super::Memory {super::Memory::default()}
    pub(super) fn io()->Option<super::ProcessIo> {None}
}

/// Times one measured region and samples working set and private bytes on its own thread.
struct Window {
    started:Instant,interval:Duration,baseline:Memory,io:Option<ProcessIo>,done:Arc<AtomicBool>,
    peaks:Arc<[AtomicU64;3]>,sampler:Option<std::thread::JoinHandle<()>>,
}
impl Window {
    fn start(interval:Duration)->Self {
        let baseline=os::memory();
        let peaks=Arc::new([AtomicU64::new(baseline.working_set.unwrap_or(0)),
            AtomicU64::new(baseline.private_bytes.unwrap_or(0)),AtomicU64::new(0)]);
        let done=Arc::new(AtomicBool::new(false));
        let (sampler_done,sampler_peaks)=(done.clone(),peaks.clone());
        let sampler=std::thread::spawn(move || loop {
            let current=os::memory();
            sampler_peaks[0].fetch_max(current.working_set.unwrap_or(0),Ordering::Relaxed);
            sampler_peaks[1].fetch_max(current.private_bytes.unwrap_or(0),Ordering::Relaxed);
            sampler_peaks[2].fetch_add(1,Ordering::Relaxed);
            if sampler_done.load(Ordering::Relaxed) {break;}
            std::thread::sleep(interval);
        });
        Self {started:Instant::now(),interval,baseline,io:os::io(),done,peaks,sampler:Some(sampler)}
    }
    fn elapsed(&self)->f64 {elapsed_ms(self.started)}
    fn stop(mut self)->(f64,Value) {
        let elapsed=self.elapsed();
        self.done.store(true,Ordering::Relaxed);
        if let Some(sampler)=self.sampler.take() {let _=sampler.join();}
        let end=os::memory();
        self.peaks[0].fetch_max(end.working_set.unwrap_or(0),Ordering::Relaxed);
        self.peaks[1].fetch_max(end.private_bytes.unwrap_or(0),Ordering::Relaxed);
        let measured=|value:Option<u64>,peak:&AtomicU64|value.map(|_|peak.load(Ordering::Relaxed));
        let io=match (self.io,os::io()) {
            (Some(before),Some(after))=>json!({"readBytes":after.read_bytes.saturating_sub(before.read_bytes),
                "writeBytes":after.write_bytes.saturating_sub(before.write_bytes),
                "readOperations":after.read_operations.saturating_sub(before.read_operations),
                "writeOperations":after.write_operations.saturating_sub(before.write_operations)}),
            _=>Value::Null,
        };
        (elapsed,json!({"workingSetAfterSetup":self.baseline.working_set,"privateBytesAfterSetup":self.baseline.private_bytes,
            "peakWorkingSet":measured(self.baseline.working_set,&self.peaks[0]),
            "peakPrivateBytes":measured(self.baseline.private_bytes,&self.peaks[1]),
            "workingSetAtEnd":end.working_set,"privateBytesAtEnd":end.private_bytes,
            "processPeakWorkingSet":end.process_peak_working_set,"processPeakPrivateBytes":end.process_peak_private_bytes,
            "samples":self.peaks[2].load(Ordering::Relaxed),"sampleIntervalMs":self.interval.as_millis() as u64,"processIo":io}))
    }
}
impl Drop for Window {
    fn drop(&mut self) {
        self.done.store(true,Ordering::Relaxed);
        if let Some(sampler)=self.sampler.take() {let _=sampler.join();}
    }
}

/// Aggregate-only counter reset. Unlike the matrix observer it registers no per-asset purposes and
/// leaves intent-input capture off, so the observers hold no per-unit inputs during a phase.
fn reset_observers() {
    crate::persistent_store::hash_work::reset_hash_work();
    crate::asset_repository::body_io::reset_body_io();
    lww::reset_work_metrics();
    crate::persistent_store::message_pages::reset_capture_work();
    server_sync::hash_metrics::reset_hash_metrics();
    lww_segment::reset_hash_bytes();
    lww_segment::reset_delegated_hash_work();
    let _=crate::external_storage::lww_engine::cycle_keys::take();
}

#[derive(Default)]
struct HashTotals(BTreeMap<String,[u64;2]>);
impl HashTotals {
    fn add(&mut self,domain:String,calls:u64,bytes:u64) {
        let entry=self.0.entry(domain).or_default();entry[0]+=calls;entry[1]+=bytes;
    }
    fn workers(&mut self,receipts:Vec<crate::external_storage::worker_observation::WorkerReceipt>)->Value {
        let (mut count,mut incomplete)=(0,0);
        for receipt in receipts {
            count+=1;if !receipt.completed {incomplete+=1;}
            for (domain,work) in receipt.hashes.domains {self.add(format!("worker/{domain}"),work.calls,work.bytes);}
        }
        json!({"workers":count,"incompleteWorkers":incomplete})
    }
}

/// Counters observed on the calling thread (and on worker scopes the product attaches to it).
fn take_observers(mut hashes:HashTotals)->Value {
    let units=lww::take_work_metrics().0;
    let native=crate::persistent_store::hash_work::take_hash_work();
    for (domain,work) in native.domains {hashes.add(format!("native/{domain}"),work.calls,work.bytes);}
    let server=server_sync::hash_metrics::take_hash_metrics();
    for (domain,work) in server.domains {hashes.add(format!("server/{domain}"),work.calls,work.bytes);}
    hashes.add("external/content-digest".into(),lww_segment::take_hash_calls(),lww_segment::take_hash_bytes());
    for (domain,(calls,bytes)) in lww_segment::take_delegated_hash_work() {hashes.add(format!("external/{domain}"),calls,bytes);}
    hashes.0.retain(|_,work|work!=&[0,0]);
    let capture=crate::persistent_store::message_pages::take_capture_work();
    let bodies=crate::asset_repository::body_io::take_body_io();
    let keys=crate::external_storage::lww_engine::cycle_keys::take();
    let domains=bodies.domains.iter().map(|(domain,work)|(domain.to_string(),json!({"openAttempts":work.open_attempts,"opens":work.opens,
        "readOperations":work.read_operations,"readBytes":work.read_bytes,"stagingWrites":work.staging_writes,
        "stagingWrittenBytes":work.staging_written_bytes,"publications":work.publications,
        "publicationBytes":work.publication_object_bytes}))).collect::<BTreeMap<_,_>>();
    let (calls,bytes)=hashes.0.values().fold((0,0),|(calls,bytes),work|(calls+work[0],bytes+work[1]));
    let key_rows=keys.selected_keys.len()+keys.attempted_keys.len()+keys.emitted_keys.len()+keys.affected_keys.len()
        +keys.held_keys.len()+keys.deferred_keys.len();
    json!({"commitUnitsVisited":units,"hashCalls":calls,"hashedBytes":bytes,
        "hashDomains":hashes.0.into_iter().map(|(domain,[calls,bytes])|(domain,json!([calls,bytes]))).collect::<BTreeMap<_,_>>(),
        "incompleteNativeHashDomains":native.incomplete.len(),"incompleteServerHash":server.incomplete,
        "messagePages":{"captures":capture.capture_calls,"failedCaptures":capture.failed_captures,"messagesRead":capture.work.messages_read,
            "bytesRead":capture.work.bytes_read,"pagesWritten":capture.work.pages_written},
        "bodies":domains,"bodyStatRequests":bodies.stat_requests,"bodyBatchStatRequests":bodies.batch_stat_requests,
        "bodyPresenceQueries":bodies.presence_queries,
        "externalCycle":{"segmentAttempts":keys.segment_attempts,"acceptedPublications":keys.accepted_publications,
            "receiveApplies":keys.receive_applies,"failedOperations":keys.failed_operations,"pendingOperations":keys.pending_operations},
        "observerRows":{"bodyObjects":bodies.objects.len(),"cycleKeys":key_rows}})
}

fn sqlite_sizes(root:&Path)->Value {
    let size=|name:&str|std::fs::metadata(root.join("persistent").join(name)).map(|metadata|metadata.len()).ok();
    json!({"persistent":size("persistent.sqlite"),"persistentWal":size("persistent.sqlite-wal"),
        "device":size("device.sqlite"),"deviceWal":size("device.sqlite-wal")})
}

fn server_io(counter:&server_sync::client::TestIoCounters)->Value {
    let [requests,sent,received]=counter.snapshot();
    json!({"requests":requests,"uploadedBytes":sent,"downloadedBytes":received})
}

/// Copies everything except CAS objects, which are hard links (copied only when linking fails).
fn fork_library(library:&Path,destination:&Path)->Result<Value,String> {
    let started=Instant::now();
    if destination.exists() {return Err(format!("fork destination {} already exists",destination.display()));}
    let objects=Path::new("assets").join("objects");
    let (mut linked,mut copied,mut copied_bytes,mut directories)=(0u64,0u64,0u64,0u64);
    let mut pending=vec![PathBuf::new()];
    while let Some(relative)=pending.pop() {
        std::fs::create_dir_all(destination.join(&relative)).text()?;directories+=1;
        for entry in std::fs::read_dir(library.join(&relative)).text()? {
            let entry=entry.text()?;let kind=entry.file_type().text()?;
            let child=relative.join(entry.file_name());
            if kind.is_symlink() {return Err(format!("rung contains a link at {}",child.display()));}
            if kind.is_dir() {pending.push(child);continue;}
            let target=destination.join(&child);
            if child.starts_with(&objects) && std::fs::hard_link(entry.path(),&target).is_ok() {linked+=1;continue;}
            copied_bytes+=std::fs::copy(entry.path(),&target).text()?;copied+=1;
        }
    }
    Ok(json!({"linkedFiles":linked,"copiedFiles":copied,"copiedBytes":copied_bytes,"directories":directories,"forkMs":elapsed_ms(started)}))
}

fn catalog(store:&PersistentStore)->Result<Vec<(String,u64)>,String> {
    let mut rows=Vec::new();let mut cursor=None;
    loop {
        let page=store.query_asset_object_catalog(4096,cursor.as_deref()).text()?;
        rows.extend(page.items.into_iter().map(|item|(item.object_hash,item.byte_size)));
        cursor=page.next_cursor;
        if cursor.is_none() {return Ok(rows);}
    }
}

/// A fresh destination holding the rung's bodies except every `missing_every`-th catalog row.
fn seed_destination(library:&Path,rows:&[(String,u64)],destination:&Path,missing_every:Option<usize>)
    ->Result<(PersistentStore,Value,Vec<(String,u64)>),String> {
    use crate::persistent_store::asset_object_catalog::AssetObjectRegistration;
    let started=Instant::now();
    let mut store=PersistentStore::open(destination).text()?;
    let (mut present,mut missing,mut shards,mut copied)=(Vec::new(),Vec::new(),BTreeSet::new(),0u64);
    for (index,(hash,size)) in rows.iter().enumerate() {
        if missing_every.is_some_and(|every|index%every==0) {missing.push((hash.clone(),*size));continue;}
        let shard=destination.join("assets").join("objects").join(&hash[..2]);
        if shards.insert(hash[..2].to_owned()) {std::fs::create_dir_all(&shard).text()?;}
        let from=library.join("assets").join("objects").join(&hash[..2]).join(&hash[2..]);
        if std::fs::hard_link(&from,shard.join(&hash[2..])).is_err() {std::fs::copy(&from,shard.join(&hash[2..])).text()?;copied+=1;}
        present.push(AssetObjectRegistration {object_hash:hash.clone(),byte_size:*size});
    }
    for chunk in present.chunks(4096) {store.asset_object_catalog().register(chunk,1).text()?;}
    Ok((store,json!({"presentObjects":present.len(),"missingObjects":missing.len(),"copiedInsteadOfLinked":copied,
        "seedMs":elapsed_ms(started)}),missing))
}

struct Settings {sample:Duration,warmups:u64,repetitions:u64,seed:u64}

struct Recorder {results:PathBuf,rung:Value,group:String,run:String}
impl Recorder {
    fn write(&self,mut line:Value)->Result<(),String> {
        let object=line.as_object_mut().ok_or("ladder line must be an object")?;
        object.insert("schema".into(),json!(LINE_SCHEMA));object.insert("rung".into(),self.rung.clone());
        object.insert("group".into(),json!(self.group));object.insert("run".into(),json!(self.run));
        object.insert("pid".into(),json!(std::process::id()));object.insert("tempDir".into(),json!(std::env::temp_dir()));object.insert("recordedAtMs".into(),json!(epoch_ms()));
        let mut bytes=serde_json::to_vec(&line).text()?;bytes.push(b'\n');
        let mut file=std::fs::OpenOptions::new().create(true).append(true).open(&self.results).text()?;
        file.write_all(&bytes).text()?;file.sync_all().text()
    }
    /// Runs one phase and records it. A failed or skipped phase still writes its line.
    fn phase(&self,phase:&str,ready:bool,run:impl FnOnce()->Result<Value,String>)->bool {
        eprintln!("LADDER-PHASE-START {} {phase}",self.group);
        let started=Instant::now();
        let outcome=if ready {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(run)).unwrap_or_else(|panic|Err(panic_text(panic)))
        } else {Err("skipped: a phase this one depends on did not complete".into())};
        let (status,mut line)=match outcome {
            Ok(value) if value.is_object()=>("ok",value),
            Ok(_)=>("failed",json!({"error":"phase result is not an object"})),
            Err(error)=>(if ready {"failed"} else {"skipped"},json!({"error":error})),
        };
        line["phase"]=json!(phase);line["status"]=json!(status);line["phaseWallMs"]=json!(elapsed_ms(started));
        if let Err(error)=self.write(line) {eprintln!("LADDER-WRITE-FAILED {phase}: {error}");}
        eprintln!("LADDER-PHASE-END {} {phase} {status} {:.0} ms",self.group,elapsed_ms(started));
        status=="ok"
    }
}

fn append_ladder_shard(store:&mut PersistentStore,shard:&native_shards::NativeShard,seed:u64)->Result<(),String> {
    use crate::asset_repository::PayloadCas;
    use crate::asset_repository::owner_manifest_codec::{encode_owner_manifest,OwnerManifestEntry};
    use crate::persistent_store::asset_object_catalog::AssetObjectRegistration;
    let cas=PayloadCas::new(store.repository_root()).text()?;
    let mut registrations=Vec::with_capacity(shard.assets.len()+1);
    let mut aliases=Vec::with_capacity(shard.assets.len());
    for asset in &shard.assets {
        let object=cas.prepare_bytes(&fixture::asset_body(seed,asset.index)).text()?;
        if object.content_hash!=asset.payload_hash || object.byte_size!=asset.byte_length {
            return Err("synthetic asset body differs from its descriptor".into());
        }
        registrations.push(AssetObjectRegistration {object_hash:object.content_hash.clone(),byte_size:object.byte_size});
        aliases.push(crate::persistent_store::AssetAlias {key:asset.logical_key.clone(),object_hash:Some(object.content_hash),
            kind:"asset".into(),size:asset.byte_length as i64,mime:"application/octet-stream".into(),
            name:format!("Synthetic asset {}",asset.index),ext:"bin".into(),inlay_type:None,width:None,height:None,metadata:json!({})});
    }
    let entries=shard.owner_entries.iter().map(|entry|Ok(OwnerManifestEntry {tuple:entry.tuple.clone(),
        payload_hash:Some(hex::decode(&entry.payload_hash).text()?.try_into().map_err(|_|"payload hash length")?)}))
        .collect::<Result<Vec<_>,String>>()?;
    let manifest=cas.prepare_bytes(&encode_owner_manifest(&entries).text()?).text()?;
    registrations.push(AssetObjectRegistration {object_hash:manifest.content_hash.clone(),byte_size:manifest.byte_size});
    for chunk in registrations.chunks(4096) {store.asset_object_catalog().register(chunk,1).text()?;}
    let owner=crate::persistent_store::AssetOwnerLocator::CharacterAdditionalAssets {
        character_id:shard.character["chaId"].as_str().ok_or("shard character id")?.into()};
    let commit=WorkingSetCommit {expected_revision:store.revision().text()?,add_character:Some(shard.character.clone()),
        asset_owner_heads:Some(vec![crate::persistent_store::AssetOwnerHead::present(owner,manifest.content_hash,entries.len() as i64)]),
        ..Default::default()};
    store.commit_with_asset_aliases(&commit,&aliases).text()?;
    Ok(())
}

fn rung_counts(store:&PersistentStore)->Result<Value,String> {
    let connection=&store.connection;
    let generation=crate::persistent_store::active_generation(connection).text()?;
    let active=|sql:&str|connection.query_row(sql,[&generation],|row|sql_u64(row,0)).text();
    let catalog:(u64,u64)=connection.query_row("SELECT COUNT(*),COALESCE(SUM(byte_size),0) FROM asset_objects",[],
        |row|Ok((sql_u64(row,0)?,sql_u64(row,1)?))).text()?;
    let pages=connection.query_row("SELECT page_count*page_size FROM pragma_page_count(),pragma_page_size()",[],|row|sql_u64(row,0)).text()?;
    Ok(json!({"characters":active("SELECT COUNT(*) FROM characters WHERE generation=?1")?,
        "conversations":active("SELECT COUNT(*) FROM conversations WHERE generation=?1")?,
        "messages":active("SELECT COUNT(*) FROM messages WHERE generation=?1")?,
        "activeRecordBytes":active("SELECT COALESCE(SUM(length(CAST(value AS BLOB))),0) FROM messages WHERE generation=?1")?
            +active("SELECT COALESCE(SUM(length(CAST(detail AS BLOB))),0) FROM characters WHERE generation=?1")?,
        "assetAliases":active("SELECT COUNT(*) FROM asset_aliases WHERE generation=?1")?,
        "ownerHeads":active("SELECT COUNT(*) FROM asset_owner_heads WHERE generation=?1 AND present=1")?,
        "catalogObjects":catalog.0,"catalogBytes":catalog.1,"databaseAllocatedBytes":pages}))
}

#[test]
#[ignore = "scaling-ladder rung construction; run through benchmarks/lww-native/ladder.ps1"]
fn ladder_construct_rung() {
    construct_rung().expect("ladder rung construction failed");
}

fn construct_rung()->Result<(),String> {
    let rung=PathBuf::from(environment("RISUNEST_LADDER_RUNG_DIR")?);
    if !rung.is_absolute() {return Err("RISUNEST_LADDER_RUNG_DIR must be absolute".into());}
    if rung.exists() && std::fs::read_dir(&rung).text()?.next().is_some() {return Err("rung directory must be new or empty".into());}
    let name=std::env::var("RISUNEST_LADDER_NAME").ok().or_else(||rung.file_name().and_then(|name|name.to_str()).map(str::to_owned))
        .ok_or("rung name")?;
    let assets=environment_u64("RISUNEST_LADDER_ASSETS",None)?;
    let per_owner=u16::try_from(environment_u64("RISUNEST_LADDER_ASSETS_PER_OWNER",Some(1_000))?)
        .map_err(|_|"RISUNEST_LADDER_ASSETS_PER_OWNER must be at most 65535")?;
    let record_bytes=environment_u64("RISUNEST_LADDER_RECORD_BYTES",None)?;
    let messages=u16::try_from(environment_u64("RISUNEST_LADDER_MESSAGES_PER_CHARACTER",Some(256))?)
        .map_err(|_|"RISUNEST_LADDER_MESSAGES_PER_CHARACTER is too large")?;
    let seed=environment_u64("RISUNEST_LADDER_SEED",Some(1))?;
    let library=rung.join("library");
    std::fs::create_dir_all(&library).text()?;
    let started=Instant::now();
    let mut store=PersistentStore::open(&library).text()?;
    let (root,presets)=fixture_configuration::NativeFixtureConfiguration::default().initial_values()?;
    let revision=store.revision().text()?;
    let staging=store.replace_begin().text()?.staging_id;
    store.replace_put_root(&staging,&root).text()?;
    store.replace_put_presets(&staging,&presets).text()?;
    store.replace_commit(&staging,Some(revision)).text()?;
    let plan=native_shards::ShardPlan {seed,assets,ordinary_messages:messages,assets_per_owner:per_owner};
    let mut shards=native_shards::NativeShards::new(plan.clone())?;
    let (mut observed_bytes,mut maximum_shard_bytes)=(0u64,0u64);
    loop {
        let shard=shards.next().ok_or("native shard ordinals exhausted")??;
        append_ladder_shard(&mut store,&shard,seed)?;
        maximum_shard_bytes=maximum_shard_bytes.max(shard.serialized_character_bytes);
        let generation=crate::persistent_store::active_generation(&store.connection).text()?;
        let character=shard.character["chaId"].as_str().ok_or("shard character id")?;
        let records:u64=store.connection.query_row(
            "SELECT COALESCE(SUM(length(CAST(value AS BLOB))),0) FROM messages WHERE generation=?1 AND character_id=?2",
            rusqlite::params![generation,character],|row|sql_u64(row,0)).text()?;
        let detail:u64=store.connection.query_row(
            "SELECT length(CAST(detail AS BLOB)) FROM characters WHERE generation=?1 AND character_id=?2",
            rusqlite::params![generation,character],|row|sql_u64(row,0)).text()?;
        observed_bytes+=records+detail;
        if shards.generated_characters()%25==0 {
            eprintln!("LADDER-CONSTRUCTION characters={} assets={}/{} recordBytes={observed_bytes} elapsedMs={:.0}",
                shards.generated_characters(),shards.assigned_assets(),assets,elapsed_ms(started));
        }
        if shards.generated_characters()>=2 && shards.assigned_assets()==assets && observed_bytes>=record_bytes {break;}
    }
    store.checkpoint(crate::persistent_store::CheckpointMode::Truncate).text()?;
    let counts=rung_counts(&store)?;
    let writer=store.lww_clock_state().text()?.writer_id;
    drop(store);
    let construction_ms=elapsed_ms(started);
    let executable=std::env::current_exe().text()?;
    let manifest=json!({"schema":RUNG_SCHEMA,"name":name,"library":"library",
        "params":{"assets":assets,"assetsPerOwner":per_owner,"recordBytesTarget":record_bytes,"messagesPerCharacter":messages,
            "firstConversationMessages":native_shards::FIRST_CONVERSATION_MESSAGES,"assetBodyBytes":fixture::BODY_BYTES,"seed":seed},
        "counts":counts,"shards":{"characters":shards.generated_characters(),"maximumShardBytes":maximum_shard_bytes,
            "observedRecordBytes":observed_bytes},
        "files":sqlite_sizes(&library),"constructionMs":construction_ms,"writerId":writer,
        "binary":{"path":executable,"bytes":std::fs::metadata(&executable).map(|metadata|metadata.len()).ok()},
        "createdAtMs":epoch_ms()});
    let file=std::fs::OpenOptions::new().create_new(true).write(true).open(rung.join("rung.json")).text()?;
    serde_json::to_writer_pretty(file,&manifest).text()?;
    eprintln!("LADDER-RUNG {}",serde_json::to_string(&manifest).text()?);
    Ok(())
}

#[test]
#[ignore = "scaling-ladder phase group; run through benchmarks/lww-native/ladder.ps1"]
fn ladder_run_group() {
    run_group().expect("ladder phase group could not start");
}

fn run_group()->Result<(),String> {
    let rung=PathBuf::from(environment("RISUNEST_LADDER_RUNG_DIR")?);
    let manifest:Value=serde_json::from_reader(File::open(rung.join("rung.json")).text()?).text()?;
    if manifest["schema"]!=RUNG_SCHEMA {return Err("rung.json is not a ladder rung".into());}
    let library=rung.join(manifest["library"].as_str().ok_or("rung library")?);
    let group=environment("RISUNEST_LADDER_GROUP")?;
    let output=match std::env::var("RISUNEST_LADDER_OUTPUT_DIR") {
        Ok(value)=>PathBuf::from(value),
        Err(_)=>Path::new(env!("CARGO_MANIFEST_DIR")).parent().ok_or("worktree root")?.join(".tmp").join("lww-ladder"),
    };
    if !output.is_absolute() {return Err("RISUNEST_LADDER_OUTPUT_DIR must be absolute".into());}
    let results=std::env::var("RISUNEST_LADDER_RESULTS").map(PathBuf::from).unwrap_or_else(|_|output.join("results.jsonl"));
    let name=manifest["name"].as_str().ok_or("rung name")?.to_owned();
    let run=format!("{name}-{group}-{}",epoch_ms());
    let work=output.join("work").join(&run);
    if work.starts_with(&rung) || rung.starts_with(&work) {return Err("work and rung directories overlap".into());}
    std::fs::create_dir_all(&work).text()?;
    if let Some(parent)=results.parent() {std::fs::create_dir_all(parent).text()?;}
    // Product staging nests deep below the temporary directory; on Windows an override longer than about 80 characters fails.
    let temporary=std::env::var("RISUNEST_LADDER_TEMP_DIR").ok().map(PathBuf::from);
    if let Some(temporary)=&temporary {
        std::fs::create_dir_all(temporary).text()?;
        std::env::set_var("TMP",temporary);std::env::set_var("TEMP",temporary);
    }
    let settings=Settings {sample:Duration::from_millis(environment_u64("RISUNEST_LADDER_SAMPLE_MS",Some(10))?.max(1)),
        warmups:environment_u64("RISUNEST_LADDER_WARMUPS",Some(1))?,repetitions:environment_u64("RISUNEST_LADDER_REPETITIONS",Some(5))?.max(1),
        seed:manifest["params"]["seed"].as_u64().unwrap_or(1)};
    let recorder=Recorder {results,group:group.clone(),run,rung:json!({"name":name,"assets":manifest["params"]["assets"],
        "assetsPerOwner":manifest["params"]["assetsPerOwner"],"recordBytesTarget":manifest["params"]["recordBytesTarget"],
        "activeRecordBytes":manifest["counts"]["activeRecordBytes"],"characters":manifest["counts"]["characters"],
        "messages":manifest["counts"]["messages"],"catalogObjects":manifest["counts"]["catalogObjects"],
        "databaseFileBytes":manifest["files"]["persistent"]})};
    let outcome=match group.as_str() {
        "server"=>{server_group(&recorder,&library,&work,&settings);Ok(())},
        "backup"=>{backup_group(&recorder,&library,&work,&settings);Ok(())},
        "maintenance"=>{maintenance_group(&recorder,&library,&work,&settings);Ok(())},
        "serverless"=>{serverless_group(&recorder,&library,&work,&settings);Ok(())},
        _=>Err("RISUNEST_LADDER_GROUP must be server, backup, maintenance or serverless".to_owned()),
    };
    if std::env::var("RISUNEST_LADDER_KEEP_WORK").as_deref()!=Ok("1") {
        if let Err(error)=std::fs::remove_dir_all(&work) {eprintln!("LADDER-CLEANUP-FAILED {}: {error}",work.display());}
    }
    outcome
}

fn queue_unit_state(store:&mut PersistentStore,limit:usize)->Result<(u64,u64),String> {
    let (mut after,mut units,mut pages)=(None,0u64,0u64);
    loop {
        let header=server_sync::lww_tests::header(store);
        let page=store.lww_queue_unit_state_page(&header,after.as_ref(),limit).text()?;
        units+=page.entries.len() as u64;pages+=1;
        if !page.has_more {return Ok((units,pages));}
        after=page.after_key;
    }
}

fn outbox_empty(store:&PersistentStore)->Result<bool,String> {
    Ok(store.lww_read_outbox(store.lww_binding_authority().text()?,1).text()?.entries.is_empty())
}

fn receive_until_idle(client:&LwwClient,store:&mut PersistentStore)->Result<(u64,u64),String> {
    let (mut cycles,mut units)=(0u64,0u64);
    loop {
        let completion=server_sync::lww_tests::receive_cycle(client,store,&[]).text()?;
        cycles+=1;units+=completion.received_units as u64;
        if completion.received_units==0 {return Ok((cycles,units));}
    }
}

fn server_group(recorder:&Recorder,library:&Path,work:&Path,settings:&Settings) {
    use crate::server_sync::residency::AssetPolicy;
    let server=LocalServerFixture::new();
    let mut source:Option<PersistentStore>=None;
    let mut sender:Option<LwwClient>=None;
    let published=recorder.phase("initial-publication",true,|| {
        let fork=fork_library(library,&work.join("source"))?;
        let mut store=PersistentStore::open(&work.join("source")).text()?;
        let client=server.client(&store);
        let config=client.client.config();
        let io=client.client.test_io.clone().ok_or("sender counters absent")?;
        io.reset();reset_observers();
        let window=Window::start(settings.sample);
        bind(&mut store,SyncTarget::Server(server.endpoint.clone()),&server.endpoint,&config.library_id);
        let bound_ms=window.elapsed();
        let (queued_units,pages)=queue_unit_state(&mut store,256)?;
        let queued_ms=window.elapsed();
        let (mut pushes,mut accepted)=(0u64,0u64);
        while let Some(receipt)=server_sync::lww_tests::publish_cycle(&client,&mut store,&[]).text()? {
            pushes+=1;accepted+=receipt.accepted_keys.len() as u64;
        }
        let (elapsed,memory)=window.stop();
        let observers=take_observers(HashTotals::default());
        let drained=outbox_empty(&store)?;
        let line=json!({"elapsedMs":elapsed,"subTimingsMs":{"bind":bound_ms,"queueUnitState":queued_ms-bound_ms,"push":elapsed-queued_ms},
            "memory":memory,"setup":fork,"counts":{"queuedUnits":queued_units,"queuePages":pages,"queuePageLimit":256,"pushes":pushes,
                "acceptedKeys":accepted,"http":server_io(&io),"outboxDrained":drained,"observers":observers},
            "sqlite":sqlite_sizes(store.repository_root())});
        if !drained {return Err(format!("outbox not drained after publication: {line}"));}
        source=Some(store);sender=Some(client);Ok(line)
    });
    let caught_up=recorder.phase("source-receive-after-publication",published,|| {
        let (store,client)=(source.as_mut().ok_or("source absent")?,sender.as_ref().ok_or("sender absent")?);
        let io=client.client.test_io.clone().ok_or("sender counters absent")?;
        io.reset();reset_observers();
        let window=Window::start(settings.sample);
        let (cycles,units)=receive_until_idle(client,store)?;
        let (elapsed,memory)=window.stop();
        Ok(json!({"elapsedMs":elapsed,"memory":memory,"counts":{"receiveCycles":cycles,"receivedUnits":units,"http":server_io(&io),
            "observers":take_observers(HashTotals::default())},"sqlite":sqlite_sizes(store.repository_root())}))
    });
    let mut remote:Option<PersistentStore>=None;
    let remote_bound=recorder.phase("bootstrap-remote",published,|| {
        let root=work.join("peer-remote");std::fs::create_dir_all(&root).text()?;
        let mut peer=PersistentStore::open(&root).text()?;
        server.prepare_binding_candidate(&peer);
        let io=Arc::new(server_sync::client::TestIoCounters::default());
        reset_observers();
        let window=Window::start(settings.sample);
        let mut activation_ms=None;
        let completion=server_sync::first_binding_cycle(&mut peer,io.clone(),|_|activation_ms=Some(window.elapsed())).text()?;
        let bound_ms=window.elapsed();
        let status=peer.asset_residency_set_policy(AssetPolicy::Remote,||Ok(())).text()?;
        let (elapsed,memory)=window.stop();
        let line=json!({"elapsedMs":elapsed,"subTimingsMs":{"databaseActivation":activation_ms,"bindingComplete":bound_ms,
            "setRemotePolicy":elapsed-bound_ms},"memory":memory,
            "counts":{"activationRevision":completion.activation.revision,"http":server_io(&io),"residency":status,
                "observers":take_observers(HashTotals::default())},"sqlite":sqlite_sizes(&root)});
        remote=Some(peer);Ok(line)
    });
    recorder.phase("residency-status-remote",remote_bound,|| {
        let peer=remote.as_ref().ok_or("remote peer absent")?;
        reset_observers();
        let window=Window::start(settings.sample);
        let status=peer.asset_residency_status().text()?;
        let (elapsed,memory)=window.stop();
        Ok(json!({"elapsedMs":elapsed,"memory":memory,"counts":{"residency":status,"remoteOrMissing":status.has_remote_or_missing(),
            "observers":take_observers(HashTotals::default())},"sqlite":sqlite_sizes(peer.repository_root())}))
    });
    drop(remote.take());
    let mut full:Option<PersistentStore>=None;
    let full_bound=recorder.phase("bootstrap-full",published,|| {
        let root=work.join("peer-full");std::fs::create_dir_all(&root).text()?;
        let mut peer=PersistentStore::open(&root).text()?;
        server.prepare_binding_candidate(&peer);
        let io=Arc::new(server_sync::client::TestIoCounters::default());
        reset_observers();
        let window=Window::start(settings.sample);
        let mut activation_ms=None;
        let completion=server_sync::first_binding_cycle(&mut peer,io.clone(),|_|activation_ms=Some(window.elapsed())).text()?;
        let bound_ms=window.elapsed();
        let objects=std::cell::Cell::new(0u64);
        server_sync::hydrate_binding_bodies(&peer,io.clone(),Some(ROUTINE_CHARACTER),||objects.set(objects.get()+1)).text()?;
        let (elapsed,memory)=window.stop();
        let observers=take_observers(HashTotals::default());
        let status=peer.asset_residency_status().text()?;
        let line=json!({"elapsedMs":elapsed,"subTimingsMs":{"databaseActivation":activation_ms,"bindingComplete":bound_ms,
            "bodies":elapsed-bound_ms},"memory":memory,
            "counts":{"activationRevision":completion.activation.revision,"hydratedObjects":objects.get(),"http":server_io(&io),
                "residencyAfter":status,"allBodiesLocal":!status.has_remote_or_missing(),"observers":observers},
            "sqlite":sqlite_sizes(&root)});
        if status.has_remote_or_missing() {return Err(format!("bodies remain remote or missing after Full bootstrap: {line}"));}
        full=Some(peer);Ok(line)
    });
    let mut receiver:Option<LwwClient>=None;
    let routine_ready=recorder.phase("peer-receive-after-bootstrap",full_bound && caught_up,|| {
        let peer=full.as_mut().ok_or("full peer absent")?;
        let io=Arc::new(server_sync::client::TestIoCounters::default());
        let client=LocalServerFixture::reopen_client(peer,io.clone()).text()?;
        reset_observers();
        let window=Window::start(settings.sample);
        let (cycles,units)=receive_until_idle(&client,peer)?;
        let (elapsed,memory)=window.stop();
        let line=json!({"elapsedMs":elapsed,"memory":memory,"counts":{"receiveCycles":cycles,"receivedUnits":units,"http":server_io(&io),
            "observers":take_observers(HashTotals::default())},"sqlite":sqlite_sizes(peer.repository_root())});
        receiver=Some(client);Ok(line)
    });
    for (phase,scenario) in [("routine-setting",measurement::Scenario::SettingEdit),("routine-append",measurement::Scenario::Append),
        ("routine-middle-edit",measurement::Scenario::MiddleEdit),("routine-burst",measurement::Scenario::Burst)] {
        recorder.phase(phase,routine_ready,|| {
            let (store,peer)=(source.as_mut().ok_or("source absent")?,full.as_mut().ok_or("full peer absent")?);
            let (sender,receiver)=(sender.as_ref().ok_or("sender absent")?,receiver.as_ref().ok_or("receiver absent")?);
            let ios=[sender.client.test_io.clone().ok_or("sender counters")?,receiver.client.test_io.clone().ok_or("receiver counters")?];
            let mut samples=Vec::new();let mut peaks=(0u64,0u64);let mut first_memory=Value::Null;
            for iteration in 0..settings.warmups+settings.repetitions {
                let prepared=prepare_final_change(store,scenario,Direction::AtoB,iteration)?;
                for io in &ios {io.reset();}
                reset_observers();
                let window=Window::start(settings.sample);
                for commit in &prepared.commits {store.commit(commit).text()?;}
                let durable=window.elapsed();
                let receipt=server_sync::lww_tests::publish_cycle(sender,store,&[]).text()?.ok_or("edit emitted no operation")?;
                let published=window.elapsed();
                let completion=server_sync::lww_tests::receive_cycle(receiver,peer,&[]).text()?;
                let (elapsed,memory)=window.stop();
                let observers=take_observers(HashTotals::default());
                peaks.0=peaks.0.max(memory["peakWorkingSet"].as_u64().unwrap_or(0));
                peaks.1=peaks.1.max(memory["peakPrivateBytes"].as_u64().unwrap_or(0));
                if first_memory.is_null() {first_memory=memory.clone();}
                verify_additional_change(peer,&prepared);
                if !outbox_empty(store)? {return Err("routine publication left an outbox entry".into());}
                samples.push(json!({"iteration":iteration,"warmup":iteration<settings.warmups,"durableMs":durable,
                    "publishMs":published,"receiveMs":elapsed,"commits":prepared.commits.len(),"acceptedKeys":receipt.accepted_keys.len(),
                    "receivedUnits":completion.received_units,"sender":server_io(&ios[0]),"receiver":server_io(&ios[1]),
                    "commitUnitsVisited":observers["commitUnitsVisited"],"hashedBytes":observers["hashedBytes"],
                    "peakWorkingSet":memory["peakWorkingSet"],"peakPrivateBytes":memory["peakPrivateBytes"]}));
            }
            let recorded=samples.iter().filter(|sample|sample["warmup"]==false).collect::<Vec<_>>();
            let pick=|field:&str|recorded.iter().filter_map(|sample|sample[field].as_f64()).collect::<Vec<_>>();
            Ok(json!({"elapsedMs":median(&pick("receiveMs")),"subTimingsMs":{"medianDurable":median(&pick("durableMs")),
                "medianPublish":median(&pick("publishMs")),"medianReceive":median(&pick("receiveMs"))},
                "memory":{"firstIteration":first_memory,"peakWorkingSet":peaks.0,"peakPrivateBytes":peaks.1},
                "counts":{"warmups":settings.warmups,"repetitions":settings.repetitions},"samples":samples,
                "sqlite":{"source":sqlite_sizes(store.repository_root()),"peer":sqlite_sizes(peer.repository_root())}}))
        });
    }
    drop((receiver,sender,full,source));
    drop(server);
}

fn backup_group(recorder:&Recorder,library:&Path,work:&Path,settings:&Settings) {
    use crate::native_file_jobs::{JobKind,JobRegistry,portable};
    let _=settings.seed;
    let mut backup:Option<(PathBuf,Vec<(String,u64)>)>=None;
    let exported=recorder.phase("device-backup",true,|| {
        let fork=fork_library(library,&work.join("source"))?;
        let store=PersistentStore::open(&work.join("source")).text()?;
        let rows=catalog(&store)?;
        let revision=store.revision().text()?;
        let (owned,handoffs)=(work.join("backup-owned"),work.join("handoffs"));
        std::fs::create_dir_all(&owned).text()?;std::fs::create_dir_all(&handoffs).text()?;
        let registry=JobRegistry::default();
        let job=registry.create_with_context(JobKind::ExportPortableBackup,Some(revision),vec![])?;
        let worker=store.open_native_job_store().text()?;
        reset_observers();
        let window=Window::start(settings.sample);
        let summary=portable::export_portable(None,revision,&owned,&handoffs,worker,&job,None,"synthetic-lww-ladder")
            .map_err(|error|format!("{}: {}",error.code,error.message))?;
        let encoded=serde_json::to_value(&summary).text()?;
        job.finish_success(summary)?;
        let (elapsed,memory)=window.stop();
        let observers=take_observers(HashTotals::default());
        let path=PathBuf::from(encoded["handoffPath"].as_str().ok_or("backup handoff path absent")?);
        let bytes=std::fs::metadata(&path).text()?.len();
        let line=json!({"elapsedMs":elapsed,"memory":memory,"setup":fork,
            "counts":{"backupBytes":bytes,"catalogObjects":rows.len(),"summary":encoded,"observers":observers},
            "sqlite":sqlite_sizes(store.repository_root())});
        backup=Some((path,rows));Ok(line)
    });
    for (phase,missing_every) in [("restore-all-present",None),("restore-missing-1pct",Some(100usize))] {
        recorder.phase(phase,exported,|| {
            let (path,rows)=backup.as_ref().ok_or("backup absent")?;
            let root=work.join(phase);std::fs::create_dir_all(&root).text()?;
            let (store,seeded,missing)=seed_destination(library,rows,&root,missing_every)?;
            portable_restore(path,&root,store,settings,seeded,rows,&missing)
        });
    }
}

/// Drives the restore job's selection, finalize and adoption gates the way the renderer does.
fn portable_restore(backup:&Path,root:&Path,store:PersistentStore,settings:&Settings,seeded:Value,rows:&[(String,u64)],missing:&[(String,u64)])
    ->Result<Value,String> {
    use crate::{native_file_jobs::{JobPhase,JobState,NativeFileJobState,OpenedJobSource,portable},portable_backup::source_io};
    let file=File::open(backup).text()?;
    let total_bytes=file.metadata().text()?.len();
    let source=OpenedJobSource {file,total_bytes,custody:None};
    let jobs=Arc::new(NativeFileJobState::initialize(root.join("native-file-jobs")));
    let persistent=Arc::new(crate::persistent_store::commands::PersistentStoreState::default());
    let device=Arc::new(crate::device_backup::DeviceBackupState::initialize(root.join("device-backup")));
    let revision=store.revision().text()?;
    let owned=root.with_extension("owned");
    std::fs::create_dir_all(&owned).text()?;
    reset_observers();source_io::reset_source_io();
    let (job,permit)=jobs.create_portable_restore_fixture(revision).map_err(|error|format!("{}: {}",error.code,error.message))?;
    let (worker_job,worker_device,worker_permit)=(Arc::clone(&job),Arc::clone(&device),Arc::clone(&permit));
    let window=Window::start(settings.sample);
    let worker=std::thread::spawn(move || {
        let _admission=worker_permit;
        reset_observers();
        let context=portable::NativePortableRestoreContext {persistent:&persistent,coordinator:&worker_device};
        let outcome=portable::restore_portable_with_context(source,false,revision,&owned,store,&worker_job,Some((&context,None)));
        let result=match outcome {
            Ok(summary)=>{
                let encoded=serde_json::to_value(&summary).text();
                worker_job.finish_success(summary).and(encoded)
            },
            Err(error)=>{
                let failed=worker_job.finish_failure(&error.code,&error.message);
                Err(format!("{}: {}{}",error.code,error.message,failed.err().map(|e|format!("; {e}")).unwrap_or_default()))
            },
        };
        (result,take_observers(HashTotals::default()))
    });
    let (mut selected,mut finalized,mut adopted,mut activation_ms,mut errors)=(false,false,false,None,Vec::new());
    let mut cleanup_failed=false;
    while !worker.is_finished() {
        let status=job.status();
        if matches!(status.state,JobState::Failed|JobState::Cancelled) {
            errors.push(format!("restore job ended as {:?}",status.state));
            if let Err(error)=jobs.begin_cleanup() {errors.push(error);cleanup_failed=true;}
            break;
        }
        let step=(||->Result<(),String> {
            if !selected && status.state==JobState::WaitingForInput && status.phase==JobPhase::AwaitingBackupSelection {
                jobs.select_portable_restore_fixture(&job.id(),portable::PortableSelection::default())
                    .map_err(|error|format!("{}: {}",error.code,error.message))?;
                selected=true;
            }
            if !finalized && status.state==JobState::WaitingForInput && status.phase==JobPhase::AwaitingActivation {
                jobs.finalize(&job.id(),Some(revision)).map_err(|error|format!("{}: {}",error.code,error.message))?;
                finalized=true;
            }
            if !adopted {
                if let Some(activated)=status.activation_revision {
                    activation_ms=Some(window.elapsed());
                    let authority=status.activation_authority.as_deref().ok_or("activation authority absent")?;
                    let session=status.device_session_id.as_deref().ok_or("activation device session absent")?;
                    device.recovery_complete(session).map_err(|error|error.message)?;
                    jobs.confirm_portable_restore_adoption(&device,&job.id(),&activated.to_string(),authority,session)
                        .map_err(|error|format!("{}: {}",error.code,error.message))?;
                    adopted=true;
                }
            }
            Ok(())
        })();
        if let Err(error)=step {
            errors.push(error);
            if let Err(error)=jobs.begin_cleanup() {errors.push(error);cleanup_failed=true;}
            break;
        }
        std::thread::yield_now();
    }
    // A failed retirement leaves the worker waiting on an input that never comes.
    if cleanup_failed {return Err(format!("restore cleanup failed: {}",errors.join("; ")));}
    let joined=worker.join();
    let (elapsed,memory)=window.stop();
    let parent=take_observers(HashTotals::default());
    let source=source_io::take_source_io();
    let (result,worker_observers)=joined.map_err(|panic|format!("restore worker panicked: {}",panic_text(panic)))?;
    let missing_hashes=missing.iter().map(|(hash,_)|hash.as_str()).collect::<BTreeSet<_>>();
    let catalog_hashes=rows.iter().map(|(hash,_)|hash.as_str()).collect::<BTreeSet<_>>();
    let present_reads=source.objects.iter().filter(|(hash,_)|catalog_hashes.contains(hash.as_str()) && !missing_hashes.contains(hash.as_str()))
        .fold((0u64,0u64),|(count,bytes),(_,work)|(count+1,bytes+work.bytes));
    let cas=crate::asset_repository::PayloadCas::new(root).text()?;
    let mut restored=0u64;
    for (hash,size) in missing {if cas.stat_object(hash).text()?==Some(*size) {restored+=1;}}
    let line=json!({"elapsedMs":elapsed,"subTimingsMs":{"activationObserved":activation_ms},"memory":memory,"setup":seeded,
        "counts":{"backupBytes":total_bytes,"selected":selected,"finalized":finalized,"adopted":adopted,"summary":result.as_ref().ok(),
            "missingObjects":missing.len(),"missingObjectsRestored":restored,
            "archiveObjectsRead":source.objects.len(),"archiveBytesRead":source.objects.values().map(|work|work.bytes).sum::<u64>(),
            "archiveObjectsReadAlreadyPresent":present_reads.0,"archiveBytesReadAlreadyPresent":present_reads.1,
            "archiveHashedBytes":source.objects.values().map(|work|work.hashed_bytes).sum::<u64>(),
            "archiveCatalogHashedBytes":source.catalog_hashed_bytes,"workerObservers":worker_observers,"ownerObservers":parent},
        "sqlite":sqlite_sizes(root)});
    if let Err(error)=result {errors.push(error);}
    if !adopted {errors.push("restore was not adopted".into());}
    if restored!=missing.len() as u64 {errors.push("missing bodies were not all restored".into());}
    if errors.is_empty() {Ok(line)} else {Err(format!("{}; line {line}",errors.join("; ")))}
}

fn maintenance_group(recorder:&Recorder,library:&Path,work:&Path,settings:&Settings) {
    use crate::persistent_store::asset_object_catalog::AssetObjectRegistration;
    let mut store:Option<PersistentStore>=None;
    let mut orphans=0u64;
    let opened=recorder.phase("residency-status",true,|| {
        let fork=fork_library(library,&work.join("library"))?;
        let mut fork_store=PersistentStore::open(&work.join("library")).text()?;
        // Unreferenced bodies older than the cleanup grace period, as an interrupted import leaves them.
        let started=Instant::now();
        let cas=crate::asset_repository::PayloadCas::new(fork_store.repository_root()).text()?;
        let count=(catalog(&fork_store)?.len() as u64/100).max(1);
        let mut registrations=Vec::new();
        for index in 0..count {
            let object=cas.prepare_bytes(&fixture::asset_body(settings.seed^0x6f72_7068_616e,index)).text()?;
            registrations.push(AssetObjectRegistration {object_hash:object.content_hash,byte_size:object.byte_size});
        }
        for chunk in registrations.chunks(4096) {fork_store.asset_object_catalog().register(chunk,1).text()?;}
        let orphan_ms=elapsed_ms(started);
        reset_observers();
        let window=Window::start(settings.sample);
        let status=fork_store.asset_residency_status().text()?;
        let (elapsed,memory)=window.stop();
        let line=json!({"elapsedMs":elapsed,"memory":memory,"setup":{"fork":fork,"orphanObjects":count,"orphanMs":orphan_ms},
            "counts":{"residency":status,"observers":take_observers(HashTotals::default())},"sqlite":sqlite_sizes(fork_store.repository_root())});
        orphans=count;store=Some(fork_store);Ok(line)
    });
    recorder.phase("storage-stats",opened,|| {
        let store=store.as_ref().ok_or("library absent")?;
        reset_observers();
        let window=Window::start(settings.sample);
        let stats=store.storage_stats().text()?;
        let (elapsed,memory)=window.stop();
        Ok(json!({"elapsedMs":elapsed,"memory":memory,"counts":{"stats":stats,"observers":take_observers(HashTotals::default())},
            "sqlite":sqlite_sizes(store.repository_root())}))
    });
    let now=epoch_ms();
    recorder.phase("cleanup-preview",opened,|| {
        let store=store.as_ref().ok_or("library absent")?;
        reset_observers();
        let window=Window::start(settings.sample);
        let preview=store.prepare_asset_gc_preview().text()?;
        let prepared=window.elapsed();
        let (mut cursor,mut pages,mut candidates,mut candidate_bytes,mut blockers,mut details)=(None,0u64,0u64,0u64,0u64,0u64);
        loop {
            let (page,detail)=store.asset_gc_preview_page_detail(&preview,128,cursor.as_deref(),now,GC_GRACE_MS).text()?;
            pages+=1;candidates+=page.report.potential_delete_hashes.len() as u64;candidate_bytes+=page.report.potential_delete_bytes;
            blockers+=page.report.blockers.len() as u64;details+=detail.len() as u64;
            match page.next_cursor {Some(next)=>cursor=Some(next),None=>break}
        }
        let (elapsed,memory)=window.stop();
        let line=json!({"elapsedMs":elapsed,"subTimingsMs":{"prepare":prepared,"pages":elapsed-prepared},"memory":memory,
            "counts":{"pages":pages,"pageLimit":128,"candidates":candidates,"candidateBytes":candidate_bytes,"blockers":blockers,
                "detailRows":details,"expectedOrphans":orphans,"observers":take_observers(HashTotals::default())},
            "sqlite":sqlite_sizes(store.repository_root())});
        if candidates!=orphans {return Err(format!("preview candidates differ from the injected orphans: {line}"));}
        Ok(line)
    });
    recorder.phase("cleanup-execute",opened,|| {
        use crate::persistent_store::{MessageObjectStore,MESSAGE_PAGE_SWEEP_LIMIT};
        let store=store.as_mut().ok_or("library absent")?;
        reset_observers();
        let window=Window::start(settings.sample);
        let marks=store.prepare_asset_gc_delete_marks().text()?;
        let prepared=window.elapsed();
        let (mut cursor,mut library_roots,mut pages,mut deleted,mut deleted_bytes,mut blockers)=(None,None,0u64,0u64,0u64,0u64);
        loop {
            let page=store.asset_gc_delete_marked_page_reusing_library(&marks,&mut library_roots,1024,cursor.as_deref(),now,GC_GRACE_MS,|_|Ok(()))
                .text()?;
            pages+=1;deleted+=page.report.deleted_hashes.len() as u64;deleted_bytes+=page.report.deleted_bytes;
            blockers+=page.report.blockers.len() as u64;
            match page.next_cursor {Some(next)=>cursor=Some(next),None=>break}
        }
        let swept_at=window.elapsed();
        let mut sweeps=0u64;
        for target in [MessageObjectStore::Library,MessageObjectStore::Device] {
            loop {
                sweeps+=1;
                if store.sweep_message_page_objects(target,now,MESSAGE_PAGE_SWEEP_LIMIT).text()?.wrapped {break;}
            }
        }
        let (elapsed,memory)=window.stop();
        let line=json!({"elapsedMs":elapsed,"subTimingsMs":{"prepare":prepared,"delete":swept_at-prepared,"messagePageSweep":elapsed-swept_at},
            "memory":memory,"counts":{"pages":pages,"pageLimit":1024,"deleted":deleted,"deletedBytes":deleted_bytes,"blockers":blockers,
                "messagePageSweeps":sweeps,"expectedOrphans":orphans,"observers":take_observers(HashTotals::default())},
            "sqlite":sqlite_sizes(store.repository_root())});
        if deleted!=orphans {return Err(format!("cleanup deleted a different count than the injected orphans: {line}"));}
        Ok(line)
    });
}

async fn connected_repository(fixture:&CycleFixture)->Result<Arc<crate::external_storage::connection_commands::ConnectedRepository>,String> {
    use crate::external_storage::{contract::{Provider,ConnectionConfig,SecretRef,OpenMode,RemoteLocator},connection_store::StoredConnection};
    let config=ConnectionConfig {provider:"synthetic".into(),profile:None,endpoint:"https://synthetic.invalid".into(),
        account_id:"fixture".into(),location:BTreeMap::new(),oauth_profile:None};
    let (handle,capabilities)=fixture.provider.open_repository(&config,&SecretRef("fixture".into()),OpenMode::Existing,&Cancellation::default())
        .await.text()?;
    let dependencies=crate::external_storage::fake::loopback_dependencies(crate::external_storage::fake::MemoryVault::default(),
        crate::external_storage::runtime::now_ms()).dependencies;
    Ok(Arc::new(crate::external_storage::connection_commands::ConnectedRepository {
        stored:StoredConnection {id:fixture.sender.connection_id.clone(),config,descriptor:fixture.sender.descriptor.clone(),
            descriptor_locator:RemoteLocator {connection_identity:handle.connection_identity.clone(),collection:None,object:"descriptor".into()},
            provider_repository_id:handle.repository_id.clone(),credential_ref:"fixture".into(),root_key_ref:"fixture-key".into(),
            recovery_key_ref:"fixture-recovery".into(),retention_policy:None,transfer_concurrency:None,capabilities,
            created_at_ms:crate::external_storage::runtime::now_ms(),verified_at_ms:crate::external_storage::runtime::now_ms(),
            last_sync_at_ms:None,last_backup_at_ms:None},provider:fixture.provider.clone(),handle,dependencies,
        root_key:zeroize::Zeroizing::new(*fixture.sender.root_key)}))
}

fn provider_io(provider:&crate::external_storage::fake::FakeProvider)->[u64;5] {
    let (sent,received)=provider.transferred_body_bytes();
    [provider.read_count() as u64,provider.upload_count() as u64,provider.listing_count() as u64,sent,received]
}
fn provider_delta(before:[u64;5],after:[u64;5])->Value {
    json!({"reads":after[0]-before[0],"uploads":after[1]-before[1],"listings":after[2]-before[2],
        "sentBodyBytes":after[3]-before[3],"receivedBodyBytes":after[4]-before[4]})
}

fn serverless_group(recorder:&Recorder,library:&Path,work:&Path,settings:&Settings) {
    let runtime=match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(runtime)=>runtime,
        Err(error)=>{let message=error.to_string();recorder.phase("serverless-publish",true,||Err(message));return;}
    };
    let mut fixture:Option<CycleFixture>=None;
    let published=recorder.phase("serverless-publish",true,|| runtime.block_on(async {
        let fork=fork_library(library,&work.join("source"))?;
        let mut cycle=CycleFixture::new();
        cycle.a=PersistentStore::open(&work.join("source")).text()?;
        cycle.sender.connection_root=cycle.a.repository_root().to_owned();
        let before=provider_io(&cycle.provider);
        reset_observers();
        let window=Window::start(settings.sample);
        bind(&mut cycle.a,SyncTarget::External(cycle.sender.connection_id.clone()),&cycle.sender.repository.connection_identity,&cycle.sender.library);
        let bound_ms=window.elapsed();
        let (queued_units,pages)=queue_unit_state(&mut cycle.a,4096)?;
        let queued_ms=window.elapsed();
        let authority=cycle.a.lww_binding_authority().text()?;
        let result=cycle.sender.publish(&mut cycle.a,authority,&[],&Cancellation::default()).await.at("publish")?;
        let (elapsed,memory)=window.stop();
        let observers=take_observers(HashTotals::default());
        let line=json!({"elapsedMs":elapsed,"subTimingsMs":{"bind":bound_ms,"queueUnitState":queued_ms-bound_ms,"publish":elapsed-queued_ms},
            "memory":memory,"setup":fork,"counts":{"queuedUnits":queued_units,"queuePages":pages,"queuePageLimit":4096,
                "segments":result.segments.0,"publishedUnits":result.units.0,"publishedBytes":result.bytes.0,
                "provider":provider_delta(before,provider_io(&cycle.provider)),"outboxDrained":outbox_empty(&cycle.a)?,"observers":observers},
            "sqlite":sqlite_sizes(cycle.a.repository_root()),"remote":"FakeProvider holds every remote object in this process's memory"});
        fixture=Some(cycle);Ok(line)
    }));
    recorder.phase("serverless-bootstrap",published,|| runtime.block_on(async {
        use crate::persistent_store::sync_selection::ReplaceBindingRequest;
        let cycle=fixture.as_ref().ok_or("serverless publisher absent")?;
        let root=work.join("serverless-peer");std::fs::create_dir_all(&root).text()?;
        let mut store=PersistentStore::open(&root).text()?;
        let connected=connected_repository(cycle).await?;
        // The new device's engine keeps its connection beside its own library, as the app does.
        let sender=&cycle.sender;
        let (repository,capabilities)={
            use crate::external_storage::contract::{Provider,SecretRef,OpenMode};
            cycle.provider.open_repository(&connected.stored.config,&SecretRef("fixture".into()),OpenMode::Existing,&Cancellation::default())
                .await.at("open repository")?
        };
        let engine=crate::external_storage::lww_engine::ExternalLwwEngine {provider:sender.provider.clone(),repository,
            library:sender.library.clone(),root_key:zeroize::Zeroizing::new(*sender.root_key),admission:sender.admission,
            connection_id:sender.connection_id.clone(),connection_root:root.clone(),capabilities,descriptor:sender.descriptor.clone()};
        crate::external_storage::connection_store::ConnectionStore::open(&root).at("connection store")?.insert(&connected.stored)
            .at("store connection")?;
        let _source=crate::external_storage::lww_residency::install_test_source_connection(&root,connected).at("source connection")?;
        let engine=&engine;
        let before=store.lww_binding_state().text()?;
        let target=SyncTarget::External(engine.connection_id.clone());
        let provider_before=provider_io(&cycle.provider);
        let cancel=Cancellation::default();
        reset_observers();
        crate::external_storage::worker_observation::begin();
        let window=Window::start(settings.sample);
        let objects=engine.listing(&cancel).await.at("listing")?;
        let listed_ms=window.elapsed();
        let snapshots=engine.snapshot_listing(&cancel).await.at("snapshot listing")?;
        let snapshots_ms=window.elapsed();
        let inspection=store.register_lww_binding_inspection(before.target_authority.clone(),&target,
            &engine.repository.connection_identity,&engine.library).at("inspection")?;
        let inspected_ms=window.elapsed();
        let stage_header=lww::Header {binding_authority:before.target_authority.clone(),request_id:uuid::Uuid::new_v4().to_string()};
        let staged=engine.stage_binding(&mut store,&stage_header,&inspection,&cancel).await.at("stage binding")?;
        let staged_ms=window.elapsed();
        let switched=store.switch_lww_binding(&SwitchBindingRequest { initial_publication: false,header:lww::Header {
            binding_authority:before.target_authority.clone(),request_id:uuid::Uuid::new_v4().to_string()},
            expected_selection_epoch:before.selection_epoch.clone(),target,inspection_id:Some(inspection)}).at("switch binding")?;
        let switched_ms=window.elapsed();
        let request=ReplaceBindingRequest {header:lww::Header {binding_authority:switched.target_authority,request_id:stage_header.request_id.clone()},
            expected_selection_epoch:switched.selection_epoch,staging_id:staged.staging_id,receive_id:stage_header.request_id,
            target_id:engine.repository.connection_identity.clone(),library_id:engine.library.clone()};
        let receipt=store.replace_lww_binding(&request).at("replace binding")?;
        let activation_ms=window.elapsed();
        let activation_steps=json!({"listing":listed_ms,"snapshotListing":snapshots_ms-listed_ms,"inspection":inspected_ms-snapshots_ms,
            "stageBinding":staged_ms-inspected_ms,"switchBinding":switched_ms-staged_ms,"replaceBinding":activation_ms-switched_ms});
        let provider_activated=provider_io(&cycle.provider);
        let authority=store.lww_binding_authority().text()?;
        let finishes=engine.receive_and_apply(&mut store,authority,&[],&cancel).await.at("receive")?;
        let received_ms=window.elapsed();
        let provider_received=provider_io(&cycle.provider);
        if store.server_asset_policy().text()?!=server_sync::residency::AssetPolicy::Full {return Err("Full policy expected".into());}
        let worker=store.open_native_job_store().text()?;
        let worker_authority=worker.lww_binding_authority().text()?;
        let identity=worker.external_identity().text()?;
        let objects_done=Arc::new(AtomicU64::new(0));
        let counted=objects_done.clone();
        crate::external_storage::worker_observation::spawn_blocking(move || {
            worker.hydrate_registered_remote_assets_prioritized(None,Some(ROUTINE_CHARACTER),|| {
                let current=worker.external_identity().map_err(|_|server_sync::SyncError::new("store-error",500))?;
                if worker.lww_binding_authority().map_err(|_|server_sync::SyncError::new("store-error",500))?!=worker_authority
                    || current.store_id!=identity.store_id || current.library_epoch!=identity.library_epoch || current.generation!=identity.generation {
                    return Err(server_sync::SyncError::new("sync-authority-changed",409));
                }
                Ok(())
            },||{counted.fetch_add(1,Ordering::Relaxed);}).at("hydrate bodies")
        }).await.text()??;
        let (elapsed,memory)=window.stop();
        let mut hashes=HashTotals::default();
        let workers=hashes.workers(crate::external_storage::worker_observation::take());
        let observers=take_observers(hashes);
        let status=store.asset_residency_status().text()?;
        let line=json!({"elapsedMs":elapsed,"subTimingsMs":{"databaseActivation":activation_ms,"databaseActivationSteps":activation_steps,
            "receive":received_ms-activation_ms,"bodies":elapsed-received_ms},"memory":memory,
            "counts":{"remoteObjects":objects.len(),"remoteSnapshots":snapshots.len(),"activationRevision":receipt.revision,
                "receiveFinishes":finishes,"hydratedObjects":objects_done.load(Ordering::Relaxed),
                "provider":provider_delta(provider_before,provider_io(&cycle.provider)),
                "providerBySubphase":{"databaseActivation":provider_delta(provider_before,provider_activated),
                    "receive":provider_delta(provider_activated,provider_received),"bodies":provider_delta(provider_received,provider_io(&cycle.provider))},
                "residencyAfter":status,
                "allBodiesLocal":!status.has_remote_or_missing(),"hydrationWorkers":workers,"observers":observers},
            "sqlite":sqlite_sizes(&root),"remote":"FakeProvider holds every remote object in this process's memory"});
        if status.has_remote_or_missing() {return Err(format!("bodies remain remote or missing after serverless bootstrap: {line}"));}
        Ok(line)
    }));
    // Stores close before their temporary directories are removed.
    if let Some(CycleFixture {directory_a,directory_b,a,b,provider,sender,receiver})=fixture {
        drop((a,b,sender,receiver,provider));drop((directory_a,directory_b));
    }
}

#[test]
#[ignore = "measurement harness self-test"]
fn ladder_fork_links_objects_and_copies_everything_else() {
    let root=tempfile::tempdir().unwrap();
    let library=root.path().join("library");
    std::fs::create_dir_all(library.join("assets/objects/ab")).unwrap();
    std::fs::create_dir_all(library.join("persistent")).unwrap();
    std::fs::write(library.join("assets/objects/ab").join("c".repeat(62)),b"body").unwrap();
    std::fs::write(library.join("persistent/persistent.sqlite"),b"database").unwrap();
    let fork=fork_library(&library,&root.path().join("fork")).unwrap();
    assert_eq!((fork["linkedFiles"].as_u64(),fork["copiedFiles"].as_u64(),fork["copiedBytes"].as_u64()),(Some(1),Some(1),Some(8)));
    let copied=root.path().join("fork/persistent/persistent.sqlite");
    std::fs::write(&copied,b"changed").unwrap();
    assert_eq!(std::fs::read(library.join("persistent/persistent.sqlite")).unwrap(),b"database");
    assert_eq!(std::fs::read(root.path().join("fork/assets/objects/ab").join("c".repeat(62))).unwrap(),b"body");
    assert!(fork_library(&library,&root.path().join("fork")).is_err());
}
