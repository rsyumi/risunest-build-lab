#![cfg(test)]

use std::{cell::RefCell, collections::BTreeMap, sync::{Arc,Mutex}};

#[derive(Clone,Debug,Default,Eq,PartialEq)]
pub(crate) struct ReadRange { pub offset:u64, pub bytes:u64 }
#[derive(Clone,Debug,Eq,PartialEq)]
pub(crate) struct CaptureMilestone { pub lease:String, pub revision:i64, pub pinned_device_meta_revision:i64 }
#[derive(Clone,Debug,Default,Eq,PartialEq)]
pub(crate) struct ObjectWork {
    pub opens:u64, pub reads:u64, pub bytes:u64, pub hashed_bytes:u64,
    pub hash_checks:u64, pub hash_failures:u64, pub incomplete:u64,
    pub outstanding:u64, pub ranges:Vec<ReadRange>,
}
#[derive(Clone,Debug,Default,Eq,PartialEq)]
pub(crate) struct CachedSqlReadWork {pub reads:u64,pub bytes:u64,pub failures:u64,pub failed_adoptions:u64}
#[derive(Clone,Debug,Default,Eq,PartialEq)]
pub(crate) struct SourceIo {
    pub metadata_ranges:Vec<ReadRange>, pub metadata_failures:u64,
    pub catalog_hashed_bytes:u64, pub objects:BTreeMap<String,ObjectWork>,
    pub catalog_hash_checks:u64, pub catalog_hash_failures:u64,
    pub pack_hashed_bytes:u64,
    pub declared_ranges:BTreeMap<String,ReadRange>,
    pub workers:BTreeMap<String,u64>,
    pub active_workers:u64, pub scope_violations:u64,
    pub captures:Vec<CaptureMilestone>,
    pub cached_sql_reads:BTreeMap<String,CachedSqlReadWork>,
}
impl SourceIo {
    pub(crate) fn complete(&self)->bool {
        self.metadata_failures==0 && self.catalog_hash_failures==0 && self.active_workers==0 && self.scope_violations==0 && self.objects.values().all(|v|v.incomplete==0 && v.outstanding==0 && v.hash_failures==0)
            && self.cached_sql_reads.values().all(|row|row.failures==0 && row.failed_adoptions==0)
    }
}
#[derive(Clone)]
pub(crate) struct Scope(Arc<Mutex<SourceIo>>,Arc<Mutex<Option<Arc<dyn Fn(&str)+Send+Sync>>>>,Arc<Mutex<Option<Arc<dyn Fn(&str,i64)+Send+Sync>>>>);
fn new_scope()->Scope {Scope(Arc::new(Mutex::new(SourceIo::default())),Arc::new(Mutex::new(None)),Arc::new(Mutex::new(None)))}
thread_local! { static CURRENT:RefCell<Scope> = RefCell::new(new_scope()); }
pub(crate) fn capture_scope()->Scope { CURRENT.with(|v|v.borrow().clone()) }
pub(crate) fn reset_source_io() { CURRENT.with(|v|*v.borrow_mut()=new_scope()); }
pub(crate) fn on_object_read(callback:impl Fn(&str)+Send+Sync+'static) { *capture_scope().1.lock().unwrap()=Some(Arc::new(callback)); }
pub(crate) fn on_capture_ready(callback:impl Fn(&str,i64)+Send+Sync+'static) { *capture_scope().2.lock().unwrap()=Some(Arc::new(callback)); }
pub(crate) fn snapshot_source_io()->SourceIo {capture_scope().0.lock().unwrap().clone()}
pub(crate) fn take_source_io()->SourceIo { let value=snapshot_source_io(); reset_source_io(); value }
pub(crate) struct Attachment(Scope,Scope);
pub(crate) fn attach(scope:&Scope)->Attachment {
    let previous=CURRENT.with(|v|v.replace(scope.clone()));
    *scope.0.lock().unwrap().workers.entry(format!("{:?}",std::thread::current().id())).or_default()+=1;
    scope.0.lock().unwrap().active_workers+=1;
    Attachment(previous,scope.clone())
}
impl Drop for Attachment { fn drop(&mut self) {self.1.0.lock().unwrap().active_workers-=1; CURRENT.with(|v|*v.borrow_mut()=self.0.clone()); } }
impl Scope {
    pub(crate) fn cached_sql_read(&self,hash:&str,result:&std::io::Result<usize>) {
        self.check_scope();let mut work=self.0.lock().unwrap();let row=work.cached_sql_reads.entry(hash.into()).or_default();row.reads+=1;
        match result {Ok(bytes)=>row.bytes+=*bytes as u64,Err(_)=>row.failures+=1}
    }
    pub(crate) fn cached_sql_adoption(&self,hash:&str,success:bool) {
        self.check_scope();if !success {self.0.lock().unwrap().cached_sql_reads.entry(hash.into()).or_default().failed_adoptions+=1;}
    }
    pub(crate) fn capture_ready(&self,lease:&str,revision:i64,pinned_device_meta_revision:i64) {
        self.check_scope();
        self.0.lock().unwrap().captures.push(CaptureMilestone{lease:lease.into(),revision,pinned_device_meta_revision});
        let callback=self.2.lock().unwrap().clone();
        if let Some(callback)=callback {callback(lease,revision);}
    }
    pub(crate) fn before_object_read(&self,hash:&str) {self.check_scope();let callback=self.1.lock().unwrap().clone();if let Some(callback)=callback {callback(hash);}}
    fn check_scope(&self) {
        let current=capture_scope();
        if !Arc::ptr_eq(&current.0,&self.0) {self.0.lock().unwrap().scope_violations+=1;current.0.lock().unwrap().scope_violations+=1;}
    }
    pub(crate) fn declare(&self,hash:&str,offset:u64,bytes:u64) {self.check_scope();self.0.lock().unwrap().declared_ranges.insert(hash.into(),ReadRange{offset,bytes});}
    pub(crate) fn metadata(&self,offset:u64,result:&std::io::Result<usize>) { self.check_scope();let mut work=self.0.lock().unwrap(); match result { Ok(bytes)=>work.metadata_ranges.push(ReadRange{offset,bytes:*bytes as u64}),Err(_)=>work.metadata_failures+=1 } }
    pub(crate) fn catalog_hash(&self,bytes:u64) {self.check_scope();self.0.lock().unwrap().catalog_hashed_bytes+=bytes;}
    pub(crate) fn catalog_checked(&self,valid:bool) {self.check_scope();let mut work=self.0.lock().unwrap();work.catalog_hash_checks+=1;if !valid {work.catalog_hash_failures+=1;}}
    pub(crate) fn pack_hash(&self,bytes:u64) {self.check_scope();self.0.lock().unwrap().pack_hashed_bytes+=bytes;}
    pub(crate) fn opened(&self,hash:&str) {self.check_scope();let mut work=self.0.lock().unwrap();let row=work.objects.entry(hash.into()).or_default();row.opens+=1;row.outstanding+=1;}
    pub(crate) fn read(&self,hash:&str,offset:u64,result:&std::io::Result<usize>) {self.check_scope();let mut work=self.0.lock().unwrap();let row=work.objects.entry(hash.into()).or_default();row.reads+=1;match result {Ok(bytes)=>{row.bytes+=*bytes as u64;row.ranges.push(ReadRange{offset,bytes:*bytes as u64});},Err(_)=>row.incomplete+=1}}
    pub(crate) fn hash_update(&self,hash:&str,bytes:u64) {self.check_scope();self.0.lock().unwrap().objects.entry(hash.into()).or_default().hashed_bytes+=bytes;}
    pub(crate) fn hashed(&self,hash:&str,_bytes:u64,valid:bool) {self.check_scope();let mut work=self.0.lock().unwrap();let row=work.objects.entry(hash.into()).or_default();row.hash_checks+=1;if !valid {row.hash_failures+=1;}}
    pub(crate) fn closed(&self,hash:&str,complete:bool) {self.check_scope();let mut work=self.0.lock().unwrap();let row=work.objects.entry(hash.into()).or_default();row.outstanding-=1;if !complete {row.incomplete+=1;}}
}
