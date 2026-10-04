use super::{contract::*, lww_checkpoint::{self, Checkpoint, PublishedCatalog}, lww_engine::{ExternalLwwEngine, read_bytes}, lww_segment as segment, packaging::RemoteObject};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use risunest_external_storage_format::{crypto::derive_key, snapshot as wire};
use crate::persistent_store::{content_capture::ContentCaptureSink, external_capture::CapturedSnapshot, sync_selection::CaptureIdentity};
use std::{collections::{BTreeMap,BTreeSet}, io::Cursor, path::Path};

#[cfg(test)]
pub(crate) struct CompactionBarrier { pub reached:tokio::sync::Notify, pub resume:tokio::sync::Notify }
#[cfg(test)]
impl CompactionBarrier {pub(crate) fn new()->std::sync::Arc<Self> {std::sync::Arc::new(Self{reached:tokio::sync::Notify::new(),resume:tokio::sync::Notify::new()})}}
#[cfg(test)]
static BARRIERS:std::sync::LazyLock<std::sync::Mutex<BTreeMap<String,std::sync::Arc<CompactionBarrier>>>>=std::sync::LazyLock::new(||std::sync::Mutex::new(BTreeMap::new()));
#[cfg(test)]
pub(crate) fn install_compaction_barrier(job_id:&str,barrier:std::sync::Arc<CompactionBarrier>) {assert!(BARRIERS.lock().unwrap().insert(job_id.into(),barrier).is_none());}
#[cfg(test)]
async fn pause_compaction(job_id:&str) {
    let barrier=BARRIERS.lock().unwrap().remove(job_id);
    if let Some(barrier)=barrier {barrier.reached.notify_one();barrier.resume.notified().await;}
}
pub(crate) struct PublishedState {
    pub catalog: PublishedCatalog,
    pub body_spool: super::capture::BackupDependencySpool,
    pub standalone: BTreeMap<String,segment::LargeBody>,
    pub standalone_roots:BTreeMap<String,String>,
    pub asset_catalogs: Vec<wire::StoredObject>,
    pub snapshots: Vec<(RemoteObject,Checkpoint)>,
    pub segments: Vec<(ObjectReceipt,segment::Segment)>,
}

pub(crate) const COMPACTION_TAKEOVER: std::time::Duration=std::time::Duration::from_secs(60);

/// Which device compacts. The device that published last starts at once; any
/// other foreground device takes over once compaction has stayed due for the
/// takeover delay without a new retained checkpoint.
#[derive(Default)]
pub(crate) struct CompactionTurn {
    due_since:Option<(std::time::Instant,BTreeSet<String>)>,
    own_publication:Option<std::time::Instant>,
    foreign_publication:Option<std::time::Instant>,
}
impl CompactionTurn {
    pub(crate) fn published(&mut self,at:std::time::Instant) {self.own_publication=Some(at)}
    pub(crate) fn observed_foreign(&mut self,at:std::time::Instant) {self.foreign_publication=Some(at)}
    /// `due` names the retained checkpoints when compaction is due.
    pub(crate) fn ready(&mut self,due:Option<BTreeSet<String>>,now:std::time::Instant)->bool {
        let Some(coverage)=due else {self.due_since=None;return false};
        if self.due_since.as_ref().is_none_or(|(_,since)|*since!=coverage) {self.due_since=Some((now,coverage));}
        let last=self.own_publication.is_some_and(|own|self.foreign_publication.is_none_or(|foreign|own>=foreign));
        last || self.due_since.as_ref().is_some_and(|(since,_)|now.saturating_duration_since(*since)>=COMPACTION_TAKEOVER)
    }
}

fn maintenance_due<C:lww_checkpoint::Covering>(checkpoints:&[C],segments:&[ObjectReceipt])->Result<bool> {
    let retained=lww_checkpoint::retained(checkpoints)?;
    if retained.len()>1 {return Ok(true);}
    let checkpoints=checkpoints.iter().filter(|checkpoint|retained.contains(checkpoint.snapshot_id()));
    let mut coverage=super::lww_checkpoint::Coverage::new();
    for checkpoint in checkpoints {
        for (writer,prefix) in checkpoint.covered_prefixes() {
            let current=coverage.entry(writer.clone()).or_insert(risunest_sync_wire::stamp::DecimalU64(0));
            *current=(*current).max(*prefix);
        }
    }
    let mut unique=BTreeMap::new();let mut bytes=0u64;let mut count=0usize;
    for receipt in segments {
        let name=receipt.locator.object.rsplit('/').next().ok_or_else(segment::corrupt)?;
        let (writer,seq,hash)=parse_segment_object_id(name)?;
        if let Some(previous)=unique.insert((writer.to_owned(),seq),hash.to_owned()) {
            if previous!=hash {return Err(segment::corrupt());}
            continue;
        }
        if seq<=coverage.get(writer).map(|prefix|prefix.0).unwrap_or(0) {continue;}
        count=count.checked_add(1).ok_or_else(segment::corrupt)?;
        bytes=bytes.checked_add(receipt.byte_length).ok_or_else(segment::corrupt)?;
    }
    Ok(count>=100 || bytes>=64*1024*1024)
}
impl PublishedState {
    pub(crate) fn require_complete(&self)->Result<()> {
        if self.segments.iter().any(|(_,segment)|self.catalog.coverage.get(&segment.writer_id).copied().unwrap_or(risunest_sync_wire::stamp::DecimalU64(0))<segment.seq) {
            return Err(ProviderError::new(ErrorKind::PreconditionFailed));
        }
        Ok(())
    }
}fn error(error: impl std::fmt::Display) -> ProviderError { segment::corrupt().caused(&error) }

struct PublishedProbe<'a>(&'a Cancellation);
impl crate::local_backup::CancellationProbe for PublishedProbe<'_> {
    fn is_cancelled(&self)->bool {self.0.check().is_err()}
}

fn live_published_objects(state:&PublishedState,capture:&mut super::capture::CaptureCatalog,anchor:Option<&str>,directory:&Path,cancel:&Cancellation)
    ->Result<(BTreeSet<String>,bool)> {
    use crate::persistent_store::external_capture::{original_unit_dependency_inventory,published_json_asset_roots,BackupBodyRole};
    use risunest_sync_wire::unit::UnitValue;
    let mut live=BTreeSet::new();let mut conservative=false;let mut controls=BTreeMap::new();let mut emitted=BTreeSet::new();
    let mut aliases=BTreeSet::new();let mut legacy=BTreeSet::new();let mut inlays=BTreeSet::new();
    fn scan(bytes:&[u8],live:&mut BTreeSet<String>,conservative:&mut bool,legacy:&mut BTreeSet<String>,inlays:&mut BTreeSet<String>)->crate::persistent_store::StoreResult<()> {
        let roots=published_json_asset_roots(bytes)?;
        live.extend(roots.object_hashes);live.extend(roots.manifest_hashes);
        *conservative|=roots.retain_all_objects || !roots.blockers.is_empty() || !roots.cold_keys.is_empty();
        legacy.extend(roots.legacy_asset_keys);inlays.extend(roots.inlay_ids);
        Ok(())
    }
    state.catalog.visit_changes(&mut |change| {
        cancel.check()?;
        if matches!(change.value,UnitValue::Deleted) {return Ok(())}
        if !crate::persistent_store::lww::lww_known_unit_key(&change.key) {conservative=true;}
        if let UnitValue::Inline{bytes}=&change.value {
            let bytes=URL_SAFE_NO_PAD.decode(bytes).map_err(error)?;
            let parts=change.key.components();
            if matches!(parts[0].as_str(),"asset"|"inlay") {aliases.insert((parts[0].clone(),parts[1].clone()));}
            scan(&bytes,&mut live,&mut conservative,&mut legacy,&mut inlays).map_err(error)?;
        }
        let units=BTreeMap::from([(change.key,change.value)]);
        let inventory=original_unit_dependency_inventory(&units,&|hash|state.body_spool.control(hash),
            &|_|Ok(None),&PublishedProbe(cancel),false,&mut |hash,bytes,role| {
                if role!=BackupBodyRole::Control {return Err(crate::persistent_store::StoreError::Validation{message:"Published control role differs".into()})}
                emitted.insert(hash.to_owned());capture.object(hash,bytes)?;
                if let Some(anchor)=anchor {capture.reference(anchor,hash,bytes.len() as u64)?;}Ok(())
            }).map_err(error)?;
        controls.extend(inventory.controls);
        live.extend(inventory.payloads.into_keys());
        // A content body can carry references that metadata alone cannot interpret.
        conservative|=!inventory.record_payloads.is_empty();
        Ok(())
    })?;
    for (hash,size) in controls {
        cancel.check()?;
        let bytes=state.body_spool.control(&hash).map_err(error)?;
        if let Some(bytes)=bytes {
            if bytes.len() as u64!=size {return Err(segment::corrupt())}
            capture.object(&hash,&bytes).map_err(error)?;
            if let Some(anchor)=anchor {capture.reference(anchor,&hash,size).map_err(error)?;}
            scan(&bytes,&mut live,&mut conservative,&mut legacy,&mut inlays).map_err(error)?;
        } else if !emitted.contains(&hash) {return Err(segment::corrupt())}
    }
    conservative|=legacy.into_iter().any(|key|!aliases.contains(&("asset".into(),key)))
        || inlays.into_iter().any(|key|!aliases.contains(&("inlay".into(),key)));
    let cas=crate::asset_repository::PayloadCas::new(directory).map_err(error)?;
    for hash in &live {
        cancel.check()?;
        if let Some(bytes)=state.body_spool.payload(hash).map_err(error)? {
            let prepared=cas.prepare_bytes(&bytes).map_err(error)?;
            if prepared.content_hash!=*hash {return Err(segment::corrupt())}
            if let Some(anchor)=anchor {capture.reference(anchor,hash,prepared.byte_size).map_err(error)?;}
        }
    }
    Ok((live,conservative))
}
/// The snapshots one connection already classified, keyed by the receipt that
/// names their bytes. An ordinary snapshot is kept as not a checkpoint.
#[derive(Default)]
pub(crate) struct CheckpointSummaries {
    scope:String,
    classified:BTreeMap<ReceiptKey,Option<lww_checkpoint::CheckpointSummary>>,
}
type ReceiptKey=(String,Option<String>,String,u64,Option<String>);
fn receipt_key(receipt:&ObjectReceipt)->ReceiptKey {
    (receipt.locator.connection_identity.clone(),receipt.locator.collection.clone(),receipt.locator.object.clone(),
        receipt.byte_length,receipt.version.as_ref().map(|version|version.0.clone()))
}
impl ExternalLwwEngine {
    /// The retained checkpoints when compaction is due.
    #[cfg(test)]
    pub(crate) async fn maintenance_needed(&self,cancel:&Cancellation)->Result<Option<BTreeSet<String>>> {
        self.maintenance_needed_cached(&Default::default(),cancel).await
    }
    /// `maintenance_needed` reading only the snapshots `cache` has not classified.
    pub(crate) async fn maintenance_needed_cached(&self,cache:&tokio::sync::Mutex<CheckpointSummaries>,cancel:&Cancellation)->Result<Option<BTreeSet<String>>> {
        let checkpoints=self.checkpoint_summaries(cache,cancel).await?;
        if !maintenance_due(&checkpoints,&self.listing(cancel).await?)? {return Ok(None)}
        Ok(Some(lww_checkpoint::retained(&checkpoints)?))
    }
    /// Every published checkpoint's summary. Only a snapshot whose receipt
    /// `cache` has not classified is read.
    pub(crate) async fn checkpoint_summaries(&self,cache:&tokio::sync::Mutex<CheckpointSummaries>,cancel:&Cancellation)->Result<Vec<lww_checkpoint::CheckpointSummary>> {
        let mut cache=cache.lock().await;
        let scope=format!("{}\n{}\n{}",self.target_scope(),self.repository.repository_id,self.descriptor.repository_id);
        if cache.scope!=scope {*cache=CheckpointSummaries{scope,classified:BTreeMap::new()};}
        let mut classified=BTreeMap::new();let mut output=Vec::new();
        for receipt in self.snapshot_receipts(cancel).await? {
            receipt.locator.validate_for(&self.repository)?;
            let key=receipt_key(&receipt);
            // An incomplete receipt is never answered from the cache, so it is refused.
            let known=if receipt.complete {cache.classified.get(&key).cloned()} else {None};
            let summary=match known {
                Some(summary)=>summary,
                None=>self.classified_checkpoint(&receipt,cancel).await?.map(|(_,checkpoint)|lww_checkpoint::CheckpointSummary::of(&checkpoint)),
            };
            output.extend(summary.clone());
            classified.insert(key,summary);
        }
        cache.classified=classified;
        Ok(output)
    }
    pub(super) async fn refresh_packed_sources(&self,sources:&[super::lww_residency::PackedSource],directory:&Path,cancel:&Cancellation)->Result<Vec<super::lww_residency::PackedSource>> {
        let mut wanted=BTreeMap::new();
        for source in sources {
            if source.library_id!=self.library || source.connection_id!=self.connection_id || source.connection_root!=self.connection_root {
                return Err(segment::corrupt());
            }
            if wanted.insert(source.hash.clone(),source.byte_length).is_some() {return Err(segment::corrupt());}
        }
        let mut checkpoints=self.checkpoints(cancel).await?.into_iter().map(|(_,checkpoint)|checkpoint).collect::<Vec<_>>();
        let retained=lww_checkpoint::retained(&checkpoints)?;
        checkpoints.sort_by(|a,b|a.snapshot_id.cmp(&b.snapshot_id));
        let mut refreshed=BTreeMap::new();let mut catalogs=BTreeMap::new();
        for checkpoint in checkpoints.iter().filter(|checkpoint|retained.contains(&checkpoint.snapshot_id)) {
            for catalog in std::iter::once(&checkpoint.library.asset_catalog).chain(&checkpoint.asset_catalogs) {
                cancel.check()?;
                if let Some(previous)=catalogs.insert(catalog.header.object_id.clone(),catalog.clone()) {
                    if previous!=*catalog {return Err(segment::corrupt());}
                    continue;
                }
                let object=RemoteObject::from_stored(catalog,&self.repository)?;
                let (entries,packs,_)=super::snapshot_restore::read_catalog(&object,wire::CatalogKind::Assets,&self.root_key,
                    directory,self.provider.as_ref(),&self.repository,cancel).await?;
                for entry in entries {
                    let hash=hex::encode(entry.content_sha256);
                    let Some(size)=wanted.get(&hash) else {continue;};
                    if *size!=entry.byte_length {return Err(segment::corrupt());}
                    let selected=entry.chunks.iter().map(|chunk|chunk.pack_id.as_str()).collect::<BTreeSet<_>>();
                    let references=packs.iter().filter(|(id,_)|selected.contains(id.as_str())).map(|(_,pack)|pack.stored(&self.repository)).collect::<Result<Vec<_>>>()?;
                    let source=super::lww_residency::PackedSource {hash:hash.clone(),byte_length:entry.byte_length,library_id:self.library.clone(),
                        connection_id:self.connection_id.clone(),connection_root:self.connection_root.clone(),protected_snapshot:checkpoint.snapshot_id.clone(),
                        catalog:catalog.clone(),chunks:entry.chunks,packs:references};
                    super::lww_residency::validate_packed_source(&source,&self.repository)?;
                    refreshed.entry(hash).or_insert(source);
                }
            }
        }
        sources.iter().map(|source|refreshed.remove(&source.hash).ok_or_else(||ProviderError::new(ErrorKind::NotFound))).collect()
    }
    async fn compact_asset_catalogs(&self,mut roots:Vec<wire::StoredObject>,live:&BTreeSet<String>,conservative:bool,
        directory:&Path,limits:super::packaging::PackageLimits,journal:&mut super::journal::TransferJournal,
        cancel:&Cancellation,protection:Option<(&super::leases::LeaseOwner,&super::leases::LeaseContext<'_>)>)
        ->Result<(Vec<wire::StoredObject>,Option<crate::asset_repository::job_pins::DurableCasJob>)> {
        use super::packaging::{EntryPlan,AssetCatalogSource};
        use crate::asset_repository::job_pins::{DurableCasJob,CasJobKind,CasObjectRole};
        if conservative {return Ok((roots,None))}
        roots.sort_by(|a,b|a.header.object_id.cmp(&b.header.object_id));
        roots.dedup_by(|a,b|a.header.object_id==b.header.object_id);
        let mut plans=BTreeMap::<String,EntryPlan>::new();let mut sources=BTreeMap::new();
        let mut pack_chunks=BTreeMap::<(String,u64), (wire::StoredChunk,bool)>::new();let mut pack_sizes=BTreeMap::new();
        for root in roots {
            cancel.check()?;
            let remote=RemoteObject::from_stored(&root,&self.repository)?;
            let (entries,packs,_)=super::snapshot_restore::read_catalog(&remote,wire::CatalogKind::Assets,
                &self.root_key,&directory.join("asset-inputs"),self.provider.as_ref(),&self.repository,cancel).await?;
            for (id,pack) in &packs {
                if pack_sizes.insert(id.clone(),pack.plaintext_length).is_some_and(|old|old!=pack.plaintext_length) {return Err(segment::corrupt())}
            }
            for entry in entries {
                let hash=hex::encode(entry.content_sha256);let retained=live.contains(&hash);
                for chunk in &entry.chunks {
                    let item=pack_chunks.entry((chunk.pack_id.clone(),chunk.offset)).or_insert((chunk.clone(),false));
                    if item.0!=*chunk {return Err(segment::corrupt())}item.1|=retained;
                }
                if !retained {continue}
                let ids=entry.chunks.iter().map(|chunk|chunk.pack_id.as_str()).collect::<BTreeSet<_>>();
                let selected=packs.iter().filter(|(id,_)|ids.contains(id.as_str())).map(|(_,pack)|pack.clone()).collect::<Vec<_>>();
                let plan=EntryPlan{kind:entry.kind,key:entry.key,content_sha256:hash.clone(),byte_length:entry.byte_length,
                    chunks:entry.chunks,packs:selected};
                plan.validate(&self.repository.repository_id,&self.repository)?;
                if let Some(old)=plans.get(&hash) {
                    if old.byte_length!=plan.byte_length {return Err(segment::corrupt())}continue;
                }
                let source=super::lww_residency::PackedSource{hash:hash.clone(),byte_length:plan.byte_length,
                    library_id:self.library.clone(),connection_id:self.connection_id.clone(),connection_root:self.connection_root.clone(),
                    protected_snapshot:remote.object_id.clone(),catalog:root.clone(),chunks:plan.chunks.clone(),
                    packs:plan.packs.iter().map(|pack|pack.stored(&self.repository)).collect::<Result<Vec<_>>>()?};
                sources.insert(hash.clone(),source);plans.insert(hash,plan);
            }
        }
        let mut pack_live=pack_sizes.into_iter().map(|(id,size)|(id,(size,0u64))).collect::<BTreeMap<_,_>>();
        for ((pack,_),(chunk,retained)) in pack_chunks {
            let item=pack_live.get_mut(&pack).ok_or_else(segment::corrupt)?;
            if retained {item.1=item.1.checked_add(chunk.stored_length).ok_or_else(segment::corrupt)?;}
            if item.1>item.0 {return Err(segment::corrupt())}
        }
        let sparse=pack_live.into_iter().filter_map(|(id,(total,live))|if live<total-live {Some(id)}else{None}).collect::<BTreeSet<_>>();
        let cas=crate::asset_repository::PayloadCas::new(&self.connection_root).map_err(error)?;
        let mut repack=Vec::new();
        for group in super::lww_residency::packed_body_groups(sources.into_values().collect(),&BTreeSet::new()) {
            if !group.iter().any(|source|source.chunks.iter().any(|chunk|sparse.contains(&chunk.pack_id))) {continue}
            let mut available=true;
            for source in &group {
                if cas.stat_object(&source.hash).map_err(error)?.is_none_or(|size|size!=source.byte_length) {available=false;break}
            }
            if available {repack.extend(group)}
        }
        let mut pins=None;
        if !repack.is_empty() {
            let id=journal.job_id();
            let mut job=match DurableCasJob::open(&self.connection_root,id) {
                Ok(job)=>job,Err(failure) if failure.kind()==std::io::ErrorKind::NotFound=>DurableCasJob::begin(&self.connection_root,id,
                    CasJobKind::OfficialPublicationOrExportPreparation,crate::asset_repository::job_pins::CasJobOwner::external_compaction(id),self.admitted_upper()? as i64).map_err(error)?,
                Err(failure)=>return Err(error(failure)),
            };
            job.pin_existing_batch(&cas,&repack.iter().map(|source|(source.hash.clone(),source.byte_length,CasObjectRole::DirectObject)).collect::<Vec<_>>()).map_err(error)?;
            pins=Some(job);
        }
        let mut catalogs=Vec::new();
        if !repack.is_empty() {
            let sources=repack.into_iter().map(|source| {
                plans.remove(&source.hash);AssetCatalogSource{content_hash:source.hash.clone(),byte_length:source.byte_length,
                    source:super::content_store::ObjectSource::Library(source.hash)}
            }).collect();
            let completed=super::packaging::package_and_upload_asset_catalog(sources,&self.connection_root,
                &directory.join("asset-repack"),&self.root_key,limits,journal,self.provider.as_ref(),&self.repository,
                &super::phase_progress::PhaseProgress::silent(),cancel,protection).await?;
            catalogs.push(completed.catalog);
        }
        if !plans.is_empty() {
            let completed=super::packaging::carry_asset_catalog(plans.into_values().collect(),&self.connection_root,
                &directory.join("asset-carry"),&self.root_key,limits,journal,self.provider.as_ref(),&self.repository,cancel).await?;
            catalogs.push(completed.catalog);
        }
        Ok((catalogs,pins))
    }
    pub(crate) async fn stage_published_objects(&self,store:&mut crate::persistent_store::PersistentStore,state:&mut PublishedState,directory:&Path,cancel:&Cancellation)->Result<()> {
        let root=store.external_lww_root();
        let cas=crate::asset_repository::PayloadCas::new(&root).map_err(error)?;
        let staged=state.body_spool.visit(&mut |hash,bytes,role| {
            if cancel.check().is_err() {return Err(crate::persistent_store::StoreError::Validation{message:"Published source staging cancelled".into()});}
            match role {
                crate::persistent_store::external_capture::BackupBodyRole::Control=>store.lww_put_object(hash,bytes)?,
                crate::persistent_store::external_capture::BackupBodyRole::Payload=>if cas.stat_object(hash)?.is_none() {store.lww_put_managed_object(hash,bytes)?;},
            }
            Ok(())
        });
        cancel.check()?;
        staged.map_err(error)?;
        for (hash,body) in &state.standalone {
            if cas.stat_object(hash).map_err(error)?.is_none() {
                super::lww_residency::register(&root,&super::lww_residency::Source {hash:hash.clone(),library_id:self.library.clone(),connection_id:self.connection_id.clone(),connection_root:self.connection_root.clone(),protected_segment:state.standalone_roots.get(hash).cloned().ok_or_else(segment::corrupt)?,body:body.clone()})?;
            }
        }
        let mut seen=BTreeSet::new();
        let retained=lww_checkpoint::retained(&state.snapshots.iter().map(|(_,snapshot)|snapshot.clone()).collect::<Vec<_>>())?;
        for (snapshot,document) in &state.snapshots {
            if !retained.contains(&document.snapshot_id) {continue;}
            let mut catalogs=document.asset_catalogs.clone(); catalogs.push(document.library.asset_catalog.clone());
            for catalog in catalogs {
                if !seen.insert(catalog.header.object_id.clone()) {continue;}
                let object=RemoteObject::from_stored(&catalog,&self.repository)?;
                let (entries,packs,_)=super::snapshot_restore::read_catalog(&object,wire::CatalogKind::Assets,&self.root_key,directory,self.provider.as_ref(),&self.repository,cancel).await?;
                let mut sources=Vec::new();
                for entry in entries {
                    let hash=hex::encode(entry.content_sha256);
                    if cas.stat_object(&hash).map_err(error)?.is_some() {continue;}
                    let selected=entry.chunks.iter().map(|c|c.pack_id.as_str()).collect::<BTreeSet<_>>();
                    let packs=packs.iter().filter(|(id,_)|selected.contains(id.as_str())).map(|(_,pack)|pack.stored(&self.repository)).collect::<Result<Vec<_>>>()?;
                    sources.push(super::lww_residency::PackedSource {hash,byte_length:entry.byte_length,library_id:self.library.clone(),connection_id:self.connection_id.clone(),connection_root:self.connection_root.clone(),protected_snapshot:snapshot.object_id.clone(),catalog:catalog.clone(),chunks:entry.chunks,packs});
                    if sources.len()==super::lww_residency::PACKED_REGISTRATION_BATCH {
                        super::lww_residency::register_packed_many(&root,&std::mem::take(&mut sources),&self.repository)?;
                    }
                }
                super::lww_residency::register_packed_many(&root,&sources,&self.repository)?;
            }
        }
        for (receipt,document) in &state.segments {
            let protected_segment=receipt.locator.object.rsplit('/').next().ok_or_else(segment::corrupt)?;
            super::snapshot_restore::admit_asset_catalogs(&document.asset_catalogs,store,&self.target_scope(),&self.library,
                protected_segment,&self.connection_id,&self.connection_root,&self.root_key,
                self.provider.as_ref(),&self.repository,cancel).await?;
        }
        Ok(())
    }
    pub(crate) async fn snapshot_listing(&self,cancel:&Cancellation) -> Result<Vec<ObjectReceipt>> {
        Ok(self.checkpoints(cancel).await?.into_iter().map(|(object,_)|object.receipt).collect())
    }
    /// Every published checkpoint, each read once.
    pub(crate) async fn checkpoints(&self,cancel:&Cancellation) -> Result<Vec<(RemoteObject,Checkpoint)>> {
        let mut output=Vec::new();
        for receipt in self.snapshot_receipts(cancel).await? {
            if let Some(found)=self.classified_checkpoint(&receipt,cancel).await? {output.push(found);}
        }
        Ok(output)
    }
    pub(crate) async fn snapshot_receipts(&self,cancel:&Cancellation) -> Result<Vec<ObjectReceipt>> {
        let mut output=Vec::new(); let mut cursor=None; let mut visited=BTreeSet::new();
        loop {
            let page=self.provider.list_objects(&self.repository,Collection::Snapshots,cursor.as_deref(),100,cancel).await?;
            output.extend(page.objects);
            match page.next_cursor { Some(next) => { if !visited.insert(next.clone()) { return Err(segment::corrupt()); } cursor=Some(next); }, None=>break }
        }
        Ok(output)
    }
    #[cfg(test)]
    pub(crate) async fn checkpoint(&self,receipt:&ObjectReceipt,cancel:&Cancellation)->Result<(RemoteObject,Checkpoint)> {self.classified_checkpoint(receipt,cancel).await?.ok_or_else(segment::corrupt)}
    pub(super) async fn classified_checkpoint(&self, receipt:&ObjectReceipt,cancel:&Cancellation) -> Result<Option<(RemoteObject,Checkpoint)>> {
        receipt.locator.validate_for(&self.repository)?;
        let bytes=read_bytes(self.provider.as_ref(),&self.repository,&receipt.locator,cancel).await?;
        let (advertised,_)=wire::read_public_header(&mut Cursor::new(&bytes)).map_err(error)?;
        let ordinary=advertised.object_id.starts_with("snapshot-");
        let repository_id=if ordinary {&self.descriptor.repository_id}else{&self.repository.repository_id};
        if advertised.repository_id!=*repository_id
            || (ordinary && !matches!(advertised.role,wire::ObjectRole::SyncState|wire::ObjectRole::BackupBundle))
            || (!ordinary && advertised.role!=wire::ObjectRole::SyncState) {return Err(segment::corrupt())}
        let key=derive_key(&self.root_key,repository_id,"metadata").map_err(error)?;
        let mut plaintext=Vec::new();
        let header=wire::open_envelope(&mut Cursor::new(&bytes),&mut plaintext,&key,wire::MAX_METADATA_BYTES as u64).map_err(error)?;
        if header!=advertised || receipt.byte_length!=bytes.len() as u64 || !receipt.complete {return Err(segment::corrupt());}
        if ordinary {
            let backup=super::control::SnapshotView::read(&plaintext,header.role,repository_id)?;
            if header.object_id!=format!("snapshot-{}",backup.snapshot_id) {return Err(segment::corrupt());}
            return Ok(None);
        }
        let document=Checkpoint::decode(&plaintext)?;
        for body in document.standalone_bodies.values() {body.locator.as_ref().ok_or_else(segment::corrupt)?.validate_for(&self.repository)?;}
        if header.repository_id!=self.repository.repository_id || header.role!=wire::ObjectRole::SyncState
            || header.object_id!=document.snapshot_id || document.repository_id!=self.repository.repository_id || document.library_id!=self.library
            || receipt.byte_length!=bytes.len() as u64 || !receipt.complete { return Err(segment::corrupt()); }
        Ok(Some((RemoteObject { repository_id:header.repository_id,object_id:header.object_id,role:ObjectRole::Snapshot,receipt:receipt.clone(),ciphertext_sha256:segment::digest(&bytes),plaintext_length:plaintext.len() as u64,plaintext_sha256:segment::digest(&plaintext) },document)))
    }
    pub(crate) async fn published_state(&self,directory:&Path,cancel:&Cancellation) -> Result<PublishedState> {
        std::fs::create_dir_all(directory).map_err(error)?;
        let mut catalog=PublishedCatalog::create(&directory.join("published.sqlite"))?;
        let snapshots=self.checkpoints(cancel).await?;
        let retained=lww_checkpoint::retained(&snapshots.iter().map(|(_,s)|s.clone()).collect::<Vec<_>>())?;
        let mut body_spool=super::capture::BackupDependencySpool::new(directory).map_err(error)?;
        let mut standalone=BTreeMap::new(); let mut standalone_roots=BTreeMap::new(); let mut asset_catalogs=Vec::new();
        for (index,(root,snapshot)) in snapshots.iter().enumerate() {
            if !retained.contains(&snapshot.snapshot_id) { continue; }
            let data=RemoteObject::from_stored(&snapshot.library.record_catalog,&self.repository)?;
            let stage=directory.join(format!("c{index}"));
            let (records,objects)=super::snapshot_restore::download_checkpoint_data(&data,&stage,&self.root_key,self.provider.as_ref(),&self.repository,cancel).await?;
            let mut proof=PublishedCatalog::create(&stage.join("proof.sqlite"))?;
            let content=super::content_store::ContentStore::open(&stage.join("external-storage")).map_err(error)?;
            for record in records {
                cancel.check()?;
                if record.byte_length > risunest_sync_wire::MAX_METADATA_BYTES as u64 {return Err(segment::corrupt());}
                let body=content.read_all(&record.content_hash).map_err(error)?;
                let change:crate::persistent_store::lww::Change=risunest_sync_wire::canonical::decode(&body,risunest_sync_wire::MAX_METADATA_BYTES).map_err(error)?;
                if record.key!=change.key.as_str() { return Err(segment::corrupt()); }
                proof.merge(&change)?; catalog.merge(&change)?;
            }
            if proof.identity()?!=snapshot.state_identity { return Err(segment::corrupt()); }
            for object in objects {
                cancel.check()?;
                let bytes=match object.source { super::content_store::ObjectSource::Captured(hash)=>content.read_all(&hash).map_err(error)?, super::content_store::ObjectSource::File(path)=>std::fs::read(path).map_err(error)?, _=>return Err(segment::corrupt()) };
                if bytes.len() as u64!=object.byte_length || segment::digest(&bytes)!=object.content_hash { return Err(segment::corrupt()); }
                body_spool.push(&object.content_hash,&bytes,crate::persistent_store::external_capture::BackupBodyRole::Control).map_err(error)?;
            }
            catalog.include_prefixes(&snapshot.covered_prefixes);
            asset_catalogs.push(snapshot.library.asset_catalog.clone());
            asset_catalogs.extend(snapshot.asset_catalogs.clone());
            for (hash,source) in &snapshot.standalone_bodies { standalone.insert(hash.clone(),source.clone()); standalone_roots.insert(hash.clone(),root.object_id.clone()); }
        }
        let mut groups:BTreeMap<(String,u64),Vec<ObjectReceipt>>=BTreeMap::new();
        for receipt in self.listing(cancel).await? {
            let name=receipt.locator.object.rsplit('/').next().ok_or_else(segment::corrupt)?;
            let (writer,seq,_)=parse_segment_object_id(name)?;
            groups.entry((writer.into(),seq)).or_default().push(receipt);
        }
        let mut segments=Vec::new();
        let mut history=PublishedCatalog::create(&directory.join("history.sqlite"))?;
        catalog.visit_changes(&mut |change|history.merge(&change))?;
        let upper=self.admitted_upper()?.checked_add(300_000).ok_or_else(segment::corrupt)?;
        for (index,((writer,seq),variants)) in groups.into_iter().enumerate() {
            let mut identity=None; let mut winner=None;
            for receipt in variants {
                let name=receipt.locator.object.rsplit('/').next().ok_or_else(segment::corrupt)?;
                let (_,_,hash)=parse_segment_object_id(name)?;
                if identity.as_deref().is_some_and(|old|old!=hash) { return Err(segment::corrupt()); }
                let bytes=read_bytes(self.provider.as_ref(),&self.repository,&receipt.locator,cancel).await?;
                if segment::digest(&bytes)!=hash { return Err(segment::corrupt()); }
                let payload=segment::open(&bytes,&self.library,&writer,seq,&self.root_key)?;
                for change in &payload.changes { if change.stamp.physical_ms.0>upper { return Err(ProviderError::new(ErrorKind::ClockSkew)); } history.merge(change)?; }
                identity=Some(hash.to_owned()); winner=Some((receipt,payload));
            }
            if let Some((receipt,mut payload))=winner {
                if catalog.include_segment(&payload)? {
                    for (hash,bytes) in &payload.message_pages { body_spool.push(hash,&URL_SAFE_NO_PAD.decode(bytes).map_err(error)?,crate::persistent_store::external_capture::BackupBodyRole::Control).map_err(error)?; }
                    let controls=super::snapshot_restore::download_control_catalogs(&payload.data_catalogs,
                        &directory.join(format!("s{index}")),&self.root_key,self.provider.as_ref(),&self.repository,cancel).await?;
                    for control in controls {
                        let super::content_store::ObjectSource::File(path)=control.source else {return Err(segment::corrupt());};
                        let bytes=std::fs::read(path).map_err(error)?;
                        if bytes.len() as u64!=control.byte_length {return Err(segment::corrupt());}
                        body_spool.push(&control.content_hash,&bytes,crate::persistent_store::external_capture::BackupBodyRole::Control).map_err(error)?;
                    }
                    asset_catalogs.extend(payload.asset_catalogs.clone());
                    for (hash,source) in &payload.large_bodies { standalone.insert(hash.clone(),source.clone()); standalone_roots.insert(hash.clone(),super::contract::segment_object_id(&payload.writer_id,payload.seq.0,&identity.clone().ok_or_else(segment::corrupt)?)?); }
                }
                payload.message_pages.clear();
                payload.changes.clear();
                segments.push((receipt,payload));
            }
        }
        body_spool.seal().map_err(error)?;
        Ok(PublishedState {catalog,body_spool,standalone,standalone_roots,asset_catalogs,snapshots,segments})
    }
    pub(crate) async fn compact_published(&self,directory:&Path,job_id:&str,writer:&str,capabilities:&super::capabilities::Capabilities,cancel:&Cancellation,protection:Option<(&super::leases::LeaseOwner,&super::leases::LeaseContext<'_>)>) -> Result<super::packaging::CompletedSnapshot> {
        let mut state=self.published_state(&directory.join("inputs"),cancel).await?;
        if let Some((owner,context))=protection {if let Some(reason)=owner.recheck(context,cancel).await? {return Err(super::leases::yield_error(reason));}}
        #[cfg(test)] pause_compaction(job_id).await;
        cancel.check()?;
        let identity=CaptureIdentity {store_id:writer.into(),library_epoch:self.library.clone(),generation:job_id.into(),selection_epoch:job_id.into(),revision:0};
        let external=directory.join("external-storage");
        let mut capture=super::capture::CaptureCatalog::create(&directory.join("capture"),&external,None).map_err(error)?;
        capture.begin(&identity,None).map_err(error)?;
        let mut first_key=None;
        let mut count=0usize;
        state.catalog.visit_changes(&mut |change| {
            cancel.check()?;
            let bytes=risunest_sync_wire::canonical::encode(&serde_json::to_value(&change).map_err(error)?).map_err(error)?;
            capture.record(change.key.as_str(),&bytes).map_err(error)?;
            if first_key.is_none() {first_key=Some(change.key.as_str().to_owned());}
            count=count.checked_add(1).ok_or_else(segment::corrupt)?;
            Ok(())
        })?;
        let (live,conservative)=live_published_objects(&state,&mut capture,first_key.as_deref(),directory,cancel)?;
        capture.finish().map_err(error)?;
        let fingerprint=capture.content_fingerprint(&risunest_external_storage_format::format::library_fingerprint_domain()).map_err(error)?;
        let mut journal=super::journal::TransferJournal::open(&directory.join("journal"),super::journal::JobIdentity{job_id:job_id.into(),connection_id:self.connection_id.clone(),repository_id:self.repository.repository_id.clone(),capture_id:job_id.into(),capture:identity.clone()}).map_err(error)?;
        let limits=super::packaging::PackageLimits::from_capabilities(capabilities)?;
        let (asset_catalogs,mut pins)=self.compact_asset_catalogs(std::mem::take(&mut state.asset_catalogs),&live,conservative,directory,limits,&mut journal,cancel,protection).await?;
        let standalone_bodies=state.standalone.into_iter().filter(|(hash,_)|conservative || live.contains(hash)).collect();
        let metadata=super::packaging::SnapshotMetadata { snapshot_id:job_id.into(),repository_id:self.repository.repository_id.clone(),library_id:self.library.clone(),author_device_id:writer.into(),created_at_ms:self.admitted_upper()?,logical_revision:0,parent_snapshot_id:None,content_fingerprint:fingerprint,
            purpose:super::packaging::SnapshotPurpose::LwwCheckpoint { covered_prefixes:state.catalog.coverage.clone(),state_identity:state.catalog.identity()?,asset_catalogs,standalone_bodies } };
        let captured=CapturedSnapshot{id:job_id.into(),identity:identity.clone(),catalog:capture,projected_records:count,shared:true};
        let completed=super::packaging::package_and_upload_protected(captured,Vec::new(),directory,&directory.join("cache"),metadata,&self.root_key,limits,None,&mut journal,self.provider.as_ref(),&self.repository,&super::phase_progress::PhaseProgress::silent(),cancel,protection).await?;
        #[cfg(test)]
        let completed={
            let mut completed=completed;
            completed.compaction_catalog_merge_visits=Some(state.catalog.visited);
            completed.compaction_capture_rows=Some(u64::try_from(count).map_err(error)?);
            completed
        };
        if let Some(pins)=pins.as_mut() {pins.release(crate::asset_repository::job_pins::CasReleaseOutcome::Aborted).map_err(error)?;}
        Ok(completed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::{fake,lww_tests::{CycleFixture,small_asset}};

    #[test]
    fn the_last_publisher_compacts_first_and_another_device_takes_over_after_sixty_seconds() {
        use std::time::{Duration,Instant};
        let start=Instant::now();
        let at=|seconds:u64|start+Duration::from_secs(seconds);
        let coverage=|id:&str|Some(BTreeSet::from([id.to_owned()]));
        let mut follower=CompactionTurn::default();
        follower.published(at(0));follower.observed_foreign(at(1));
        assert!(!follower.ready(coverage("a"),at(2)));
        assert!(!follower.ready(coverage("a"),at(61)));
        assert!(follower.ready(coverage("a"),at(62)),"no new checkpoint for sixty seconds");
        assert!(!follower.ready(coverage("b"),at(70)),"a new retained checkpoint is published progress");
        assert!(!follower.ready(coverage("b"),at(129)));
        assert!(follower.ready(coverage("b"),at(130)));
        assert!(!follower.ready(None,at(131)));
        assert!(!follower.ready(coverage("b"),at(132)),"compaction that stopped being due starts a new wait");
        let mut fresh=CompactionTurn::default();
        assert!(!fresh.ready(coverage("a"),at(0)),"a restarted device does not know it published last");
        assert!(fresh.ready(coverage("a"),at(60)));
        let mut leader=CompactionTurn::default();
        leader.observed_foreign(at(0));leader.published(at(1));
        assert!(leader.ready(coverage("a"),at(2)));
        assert!(!leader.ready(None,at(3)));
    }

    #[test]
    fn current_published_payload_is_captured_and_unreferenced_payload_is_left_out() {
        use risunest_sync_wire::{unit::{UnitKey,UnitValue},descriptor::RecordDescriptor,stamp::{Stamp,DecimalU64}};
        let directory=tempfile::tempdir().unwrap();let bytes=b"\"synthetic current data\"";let dead=b"\"synthetic retired data\"";
        let hash=risunest_sync_wire::hash(bytes);let dead_hash=risunest_sync_wire::hash(dead);
        let mut catalog=PublishedCatalog::create(&directory.path().join("published.sqlite")).unwrap();
        catalog.merge(&crate::persistent_store::lww::Change{key:UnitKey::new(&["future-unit","current"]).unwrap(),
            stamp:Stamp{physical_ms:DecimalU64(1),logical:0,writer_id:"00000000-0000-4000-8000-000000000001".into()},
            value:UnitValue::object(RecordDescriptor::content(hash.clone())).unwrap()}).unwrap();
        let mut body_spool=super::super::capture::BackupDependencySpool::new(directory.path()).unwrap();
        for (hash,body) in [(&hash,bytes.as_slice()),(&dead_hash,dead.as_slice())] {
            body_spool.push(hash,body,crate::persistent_store::external_capture::BackupBodyRole::Payload).unwrap();
        }
        body_spool.seal().unwrap();
        let state=PublishedState{catalog,body_spool,standalone:Default::default(),standalone_roots:Default::default(),
            asset_catalogs:vec![],snapshots:vec![],segments:vec![]};
        let mut capture=super::super::capture::CaptureCatalog::create(&directory.path().join("external-storage/captures/current-payload"),&directory.path().join("external-storage"),None).unwrap();
        let identity=CaptureIdentity{store_id:"synthetic".into(),library_epoch:"synthetic".into(),generation:"synthetic".into(),selection_epoch:"synthetic".into(),revision:0};
        capture.begin(&identity,None).unwrap();capture.record("current",b"synthetic record").unwrap();
        let (live,_)=live_published_objects(&state,&mut capture,Some("current"),directory.path(),&Cancellation::default()).unwrap();
        capture.finish().unwrap();
        let cas=crate::asset_repository::PayloadCas::new(directory.path()).unwrap();
        assert_eq!(cas.read_object(&hash).unwrap().unwrap(),bytes);
        assert!(cas.stat_object(&dead_hash).unwrap().is_none());assert!(live.contains(&hash));assert!(!live.contains(&dead_hash));
        let reference=capture.durable_reference("synthetic",directory.path()).unwrap();
        let roots=super::super::capture::registered_capture_roots([&reference],directory.path()).unwrap();
        assert!(roots.assets.object_hashes.contains(&hash));assert!(!roots.assets.object_hashes.contains(&dead_hash));
    }

    fn deepest_path(directory:&Path)->usize {
        let mut deepest=directory.as_os_str().len();
        for entry in std::fs::read_dir(directory).unwrap() {
            let path=entry.unwrap().path();
            deepest=deepest.max(if path.is_dir() {deepest_path(&path)} else {path.as_os_str().len()});
        }
        deepest
    }

    #[test]
    fn maintenance_compaction_stays_within_windows_path_limits_under_a_long_app_data_root() {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
            use crate::persistent_store::{WorkingSetCommit,ConversationMutation,lww::UnitMutation};
            use risunest_sync_wire::unit::UnitKey;
            let mut f=CycleFixture::new();let cancel=Cancellation::default();
            f.a.commit(&WorkingSetCommit{expected_revision:f.a.revision().unwrap(),unit_mutations:Some(vec![
                UnitMutation::Set{key:UnitKey::new(&["exists","character","char"]).unwrap(),value:serde_json::json!({"type":"character"})},
                UnitMutation::Set{key:UnitKey::new(&["exists","conversation","char","conv"]).unwrap(),value:serde_json::json!(true)},
            ]),..Default::default()}).unwrap();
            let messages=(0..40).map(|index|serde_json::json!({"chatId":format!("synthetic-depth-{index}"),"data":"x".repeat(128*1024)})).collect();
            f.a.commit(&WorkingSetCommit{expected_revision:f.a.revision().unwrap(),conversations:Some(vec![ConversationMutation::ReplaceRange{
                character_id:"char".into(),conversation_id:"conv".into(),start:0,delete_count:0,messages,conversation:None,configured_index:None,
            }]),..Default::default()}).unwrap();
            f.publish_a().await;
            let writer=f.a.lww_clock_state().unwrap().writer_id;
            let published=f.sender.listing(&cancel).await.unwrap().remove(0);
            let (_,seq,_)=parse_segment_object_id(&published.locator.object).unwrap();
            let payload=segment::open(&f.provider.contents(&published.locator.object).unwrap(),&f.sender.library,&writer,seq,&f.sender.root_key).unwrap();
            assert_eq!(payload.data_catalogs.len(),1,"the compacted segment carries a data catalog");
            // macOS puts the default temporary directory under a long /var/folders path.
            let base=if cfg!(windows) {tempfile::tempdir()} else {tempfile::tempdir_in("/tmp")}.unwrap();
            let base_length=base.path().as_os_str().len();
            assert!(base_length<59,"temporary directory {base_length} characters long");
            let root=base.path().join("r".repeat(66-base_length-1));
            assert_eq!(root.as_os_str().len(),66);
            let first="00000000-0000-4000-8000-000000000084";
            let directory=root.join("external-storage").join("maintenance").join(first);
            let completed=f.sender.compact_published(&directory,first,&writer,&fake::capabilities(true),&cancel,None).await.unwrap();
            let (_,checkpoint)=f.sender.checkpoint(&completed.reference.receipt,&cancel).await.unwrap();
            assert_eq!(checkpoint.covered_prefixes.get(&writer).unwrap().0,seq);
            f.a.commit(&WorkingSetCommit{expected_revision:f.a.revision().unwrap(),unit_mutations:Some(vec![
                UnitMutation::Set{key:UnitKey::new(&["root","language"]).unwrap(),value:serde_json::json!("en")},
            ]),..Default::default()}).unwrap();
            f.publish_a().await;
            let second="00000000-0000-4000-8000-000000000083";
            let again=root.join("external-storage").join("maintenance").join(second);
            let completed=f.sender.compact_published(&again,second,&writer,&fake::capabilities(true),&cancel,None).await.unwrap();
            let (_,checkpoint)=f.sender.checkpoint(&completed.reference.receipt,&cancel).await.unwrap();
            assert_eq!(checkpoint.covered_prefixes.get(&writer).unwrap().0,seq+1);
            let deepest=deepest_path(&root);
            assert!(deepest<260,"staging path reaches {deepest} characters");
        });
    }

    #[test]
    fn compaction_prunes_dead_assets_and_repacks_only_locally_available_sparse_packs() {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
            for local in [false,true] {
                let mut f=CycleFixture::new();let cancel=Cancellation::default();let mut hashes=Vec::new();
                for n in 0..4 {hashes.push(small_asset(&mut f.a,&format!("asset-{n}"),format!("synthetic pack body {n}").as_bytes()));}
                use crate::persistent_store::{WorkingSetCommit,ConversationMutation,lww::UnitMutation};
                use risunest_sync_wire::unit::UnitKey;
                f.a.commit(&WorkingSetCommit{expected_revision:f.a.revision().unwrap(),unit_mutations:Some(vec![
                    UnitMutation::Set{key:UnitKey::new(&["exists","character","char"]).unwrap(),value:serde_json::json!({"type":"character"})},
                    UnitMutation::Set{key:UnitKey::new(&["exists","conversation","char","conv"]).unwrap(),value:serde_json::json!(true)},
                ]),..Default::default()}).unwrap();
                f.a.commit(&WorkingSetCommit{expected_revision:f.a.revision().unwrap(),conversations:Some(vec![ConversationMutation::ReplaceRange{
                    character_id:"char".into(),conversation_id:"conv".into(),start:0,delete_count:0,
                    messages:vec![serde_json::json!({"data":"synthetic retained message","chatId":"synthetic-message"})],
                    conversation:None,configured_index:None,
                }]),..Default::default()}).unwrap();
                f.publish_a().await;
                let input=tempfile::tempdir().unwrap();
                let state=f.sender.published_state(input.path(),&cancel).await.unwrap();
                assert_eq!(state.asset_catalogs.len(),1);
                let root=RemoteObject::from_stored(&state.asset_catalogs[0],&f.sender.repository).unwrap();
                let (_,old_packs,_)=super::super::snapshot_restore::read_catalog(&root,wire::CatalogKind::Assets,&[7;32],
                    input.path(),f.provider.as_ref(),&f.sender.repository,&cancel).await.unwrap();
                assert_eq!(old_packs.len(),1);
                for n in 1..4 {f.a.delete_asset_alias("asset",&format!("asset-{n}"),f.a.revision().unwrap()).unwrap();}
                f.publish_a().await;
                let unpublished=small_asset(&mut f.a,"unpublished",b"unpublished native asset");
                let before=old_packs.keys().map(|id|(id.clone(),f.provider.read_attempts(id))).collect::<BTreeMap<_,_>>();
                let engine=if local {&f.sender}else{&f.receiver};
                let work=tempfile::tempdir().unwrap();
                let completed=engine.compact_published(work.path(),"00000000-0000-4000-8000-000000000087",
                    &f.a.lww_clock_state().unwrap().writer_id,&fake::capabilities(true),&cancel,None).await.unwrap();
                assert!(completed.compaction_capture_rows.unwrap()>0);
                assert!(completed.compaction_catalog_merge_visits.unwrap()>=completed.compaction_capture_rows.unwrap());
                let (_,checkpoint)=engine.checkpoint(&completed.reference.receipt,&cancel).await.unwrap();
                assert!(engine.maintenance_needed(&cancel).await.unwrap().is_none());
                if local {
                    engine.compact_published(&work.path().join("equivalent"),"00000000-0000-4000-8000-000000000086",
                        &f.a.lww_clock_state().unwrap().writer_id,&fake::capabilities(true),&cancel,None).await.unwrap();
                    assert!(engine.maintenance_needed(&cancel).await.unwrap().is_none(),"equal-coverage checkpoints awaiting grace do not trigger compaction");
                    let writer=f.a.lww_clock_state().unwrap().writer_id;
                    let mut covered=checkpoint.clone();covered.covered_prefixes.insert(writer.clone(),risunest_sync_wire::stamp::DecimalU64(100));
                    let receipts=(1..=100).map(|seq| {let mut receipt=completed.reference.receipt.clone();
                        receipt.locator.object=segment_object_id(&writer,seq,&"a".repeat(64)).unwrap();receipt.byte_length=1024*1024;receipt}).collect::<Vec<_>>();
                    assert!(!maintenance_due(&[covered.clone()],&receipts).unwrap(),"covered grace history is excluded from both trigger budgets");
                    let mut dominated=covered.clone();dominated.snapshot_id="00000000-0000-4000-8000-000000000085".into();
                    dominated.covered_prefixes.insert(writer.clone(),risunest_sync_wire::stamp::DecimalU64(101));
                    assert!(!maintenance_due(&[covered,dominated],&receipts).unwrap());
                }
                assert_eq!(checkpoint.asset_catalogs.len(),1);
                let root=RemoteObject::from_stored(&checkpoint.asset_catalogs[0],&engine.repository).unwrap();
                let (entries,packs,_)=super::super::snapshot_restore::read_catalog(&root,wire::CatalogKind::Assets,&[7;32],
                    work.path(),f.provider.as_ref(),&engine.repository,&cancel).await.unwrap();
                assert_eq!(entries.len(),1);assert_eq!(hex::encode(entries[0].content_sha256),hashes[0]);
                assert_ne!(hex::encode(entries[0].content_sha256),unpublished);
                assert_eq!(packs.keys().collect::<Vec<_>>()==old_packs.keys().collect::<Vec<_>>(),!local);
                for (id,reads) in before {assert_eq!(f.provider.read_attempts(&id),reads,"compaction never fetches old Asset packs");}
                for old in old_packs.values() {assert!(f.provider.state.lock().unwrap().objects.contains_key(&old.object_id),"old packs remain until retention/grace GC");}
                for segment in engine.listing(&cancel).await.unwrap() {f.provider.delete_object(&engine.repository,&segment.locator,&cancel).await.unwrap();}
                assert!(f.receive_b().await>0);
                let restored=f.b.materialize(None).unwrap();
                assert_eq!(restored["characters"][0]["chats"][0]["message"][0]["data"],"synthetic retained message");
            }
        });
    }
}
