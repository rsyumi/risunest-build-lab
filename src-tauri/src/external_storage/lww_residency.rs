use super::{
    connection_store::ConnectionStore,
    contract::*,
    lww_segment::{self as segment, LargeBody},
};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Source {
    pub hash: String,
    pub library_id: String,
    pub connection_id: String,
    pub connection_root: PathBuf,
    pub protected_segment: String,
    pub body: LargeBody,
}
#[derive(Clone,Serialize,Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
pub(crate) struct PackedSource {
    pub hash:String,
    pub byte_length:u64,
    pub library_id:String,
    pub connection_id:String,
    pub connection_root:PathBuf,
    pub protected_snapshot:String,
    pub catalog:risunest_external_storage_format::snapshot::StoredObject,
    pub chunks:Vec<risunest_external_storage_format::snapshot::StoredChunk>,
    pub packs:Vec<risunest_external_storage_format::snapshot::StoredObject>,
}
#[derive(Clone,Serialize,Deserialize)]
#[serde(tag="kind",rename_all="kebab-case",deny_unknown_fields)]
pub(crate) enum FrozenBodySource {
    Standalone(Source),
    Packed(PackedSource),
}
impl FrozenBodySource {
    pub(crate) fn byte_length(&self)->u64 {
        match self {
            Self::Standalone(source)=>source.body.plaintext_byte_length.0,
            Self::Packed(source)=>source.byte_length,
        }
    }
}
pub(crate) fn freeze_remote_body(root:&Path,hash:&str)->Result<Option<FrozenBodySource>> {
    if let Some(source)=source(root,hash)? {return Ok(Some(FrozenBodySource::Standalone(source)));}
    Ok(packed_source(root,hash)?.map(FrozenBodySource::Packed))
}
fn local(error: impl std::fmt::Display) -> ProviderError {
    segment::corrupt().caused(&error)
}
fn database(root: &Path, create: bool) -> Result<Option<Connection>> {
    let directory = root.join("external-sync");
    let path = directory.join("remote-bodies.sqlite");
    if !create && !path.exists() {
        return Ok(None);
    }
    std::fs::create_dir_all(&directory).map_err(local)?;
    if crate::trust_boundary::is_link_like(&std::fs::symlink_metadata(&directory).map_err(local)?) {
        return Err(segment::corrupt());
    }
    for suffix in ["", "-wal", "-shm"] {
        let path = PathBuf::from(format!("{}{suffix}", path.display()));
        if path.exists() {
            crate::trust_boundary::open_regular_source(&path).map_err(local)?;
        }
    }
    let db = Connection::open(path).map_err(local)?;
    db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA busy_timeout=5000;
        CREATE TABLE IF NOT EXISTS sources(hash TEXT NOT NULL,library TEXT NOT NULL,source TEXT NOT NULL,PRIMARY KEY(hash,library));
        CREATE TABLE IF NOT EXISTS packed_sources(hash TEXT NOT NULL,library TEXT NOT NULL,source TEXT NOT NULL,PRIMARY KEY(hash,library));").map_err(local)?;
    Ok(Some(db))
}
pub(crate) fn register(root: &Path, source: &Source) -> Result<()> {
    risunest_sync_wire::validate_hash(&source.hash).map_err(local)?;
    let locator = source.body.locator.as_ref().ok_or_else(segment::corrupt)?;
    if source.protected_segment.is_empty()
        || locator.object.is_empty()
        || source.connection_id.is_empty()
    {
        return Err(segment::corrupt());
    }
    let db = database(root, true)?.ok_or_else(segment::corrupt)?;
    let previous: Option<String> = db
        .query_row(
            "SELECT source FROM sources WHERE hash=?1 AND library=?2",
            params![source.hash, source.library_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(local)?;
    if let Some(previous) = previous {
        let previous: Source = serde_json::from_str(&previous).map_err(local)?;
        if previous.body.plaintext_byte_length != source.body.plaintext_byte_length {
            return Err(segment::corrupt());
        }
    }
    db.execute("INSERT INTO sources(hash,library,source) VALUES(?1,?2,?3) ON CONFLICT(hash,library) DO UPDATE SET source=excluded.source",
        params![source.hash,source.library_id,serde_json::to_string(source).map_err(local)?]).map_err(local)?;
    Ok(())
}
pub(crate) fn source(root: &Path, hash: &str) -> Result<Option<Source>> {
    let Some(db) = database(root, false)? else {
        return Ok(None);
    };
    let value: Option<String> = db
        .query_row(
            "SELECT source FROM sources WHERE hash=?1 ORDER BY library LIMIT 1",
            [hash],
            |r| r.get(0),
        )
        .optional()
        .map_err(local)?;
    value
        .map(|value| serde_json::from_str(&value).map_err(local))
        .transpose()
}
pub(crate) fn stat(root: &Path, hash: &str) -> std::io::Result<Option<u64>> {
    source(root, hash)
        .and_then(|source| if let Some(source)=source {Ok(Some(source.body.plaintext_byte_length.0))} else {packed_source(root,hash).map(|s|s.map(|s|s.byte_length))})
        .map_err(|_| std::io::Error::other("external-source-invalid"))
}
pub(crate) fn validate_packed_source(source:&PackedSource,repository:&RepositoryHandle) -> Result<()> {
    risunest_sync_wire::validate_hash(&source.hash).map_err(local)?;
    source.catalog.validate().map_err(local)?;
    super::packaging::RemoteObject::from_stored(&source.catalog,repository)?;
    if source.catalog.header.role!=risunest_external_storage_format::snapshot::ObjectRole::Catalog
        || source.protected_snapshot.is_empty() || source.connection_id.is_empty()
        || (source.chunks.is_empty() && source.byte_length!=0) { return Err(segment::corrupt()); }
    let mut length=0u64;
    let packs=source.packs.iter().map(|p|(p.header.object_id.as_str(),p)).collect::<std::collections::BTreeMap<_,_>>();
    if packs.len()!=source.packs.len() {return Err(segment::corrupt());}
    for chunk in &source.chunks {
        let pack=packs.get(chunk.pack_id.as_str()).ok_or_else(segment::corrupt)?;
        super::packaging::RemoteObject::from_stored(pack,repository)?;
        if pack.header.role!=risunest_external_storage_format::snapshot::ObjectRole::Pack
            || pack.header.repository_id!=source.catalog.header.repository_id
            || chunk.offset.checked_add(chunk.stored_length).is_none_or(|end|end>pack.plaintext_length) { return Err(segment::corrupt()); }
        length=length.checked_add(chunk.plaintext_length).ok_or_else(segment::corrupt)?;
    }
    if length!=source.byte_length { return Err(segment::corrupt()); }
    Ok(())
}
pub(crate) const PACKED_REGISTRATION_BATCH:usize=1024;
pub(crate) fn register_packed(root:&Path,source:&PackedSource,repository:&RepositoryHandle) -> Result<()> {
    register_packed_many(root,std::slice::from_ref(source),repository)
}
pub(crate) fn register_packed_many(root:&Path,sources:&[PackedSource],repository:&RepositoryHandle) -> Result<()> {
    for source in sources {validate_packed_source(source,repository)?;}
    register_verified_packed_many(root,sources)
}
/// Registers the sources through one connection, committing every batch, so a large catalog
/// does not pay a connection and a synchronous commit per body.
pub(super) fn register_verified_packed_many<'a>(root:&Path,sources:impl IntoIterator<Item=&'a PackedSource>) -> Result<()> {
    let mut sources=sources.into_iter().peekable();
    if sources.peek().is_none() {return Ok(());}
    let mut db=database(root,true)?.ok_or_else(segment::corrupt)?;
    while sources.peek().is_some() {
        let transaction=db.transaction().map_err(local)?;
        for source in sources.by_ref().take(PACKED_REGISTRATION_BATCH) {
            transaction.execute("INSERT INTO packed_sources VALUES(?1,?2,?3) ON CONFLICT(hash,library) DO UPDATE SET source=excluded.source",params![source.hash,source.library_id,serde_json::to_string(source).map_err(local)?]).map_err(local)?;
        }
        transaction.commit().map_err(local)?;
    }
    Ok(())
}
pub(crate) fn packed_source(root:&Path,hash:&str) -> Result<Option<PackedSource>> {
    let Some(db)=database(root,false)? else {return Ok(None)};
    let value:Option<String>=db.query_row("SELECT source FROM packed_sources WHERE hash=?1 ORDER BY library LIMIT 1",[hash],|r|r.get(0)).optional().map_err(local)?;
    value.map(|v|serde_json::from_str(&v).map_err(local)).transpose()
}
pub(crate) async fn fulfill(
    root: &Path,
    hash: &str,
    provider: &dyn Provider,
    repository: &RepositoryHandle,
    key: &[u8; 32],
    cancel: &Cancellation,
) -> Result<Option<std::fs::File>> {
    let cas = crate::asset_repository::PayloadCas::new(root).map_err(local)?;
    if let Some(file) = cas.open_object(hash).map_err(local)? {
        return Ok(Some(file));
    }
    let Some(bytes) = verified_body(root, hash, provider, repository, key, cancel).await? else {
        return Ok(None);
    };
    cancel.check()?;
    promote(root, hash, &bytes)
}
async fn verified_body(
    root: &Path,
    hash: &str,
    provider: &dyn Provider,
    repository: &RepositoryHandle,
    key: &[u8; 32],
    cancel: &Cancellation,
) -> Result<Option<Vec<u8>>> {
    let Some(source) = source(root, hash)? else {
        let Some(source)=packed_source(root,hash)? else {return Ok(None)};
        let staging=tempfile::tempdir().map_err(local)?;
        return super::snapshot_restore::download_packed_body(hash,source.byte_length,source.chunks,source.packs,staging.path(),key,provider,repository,cancel).await.map(Some);
    };
    let locator = source.body.locator.as_ref().ok_or_else(segment::corrupt)?;
    locator.validate_for(repository)?;
    let sealed = super::lww_engine::read_bytes(provider, repository, locator, cancel).await?;
    if sealed.len() as u64 != source.body.byte_length.0
        || segment::digest(&sealed) != source.body.sha256
    {
        return Err(segment::corrupt());
    }
    let bytes = segment::open_body(&sealed, &source.library_id, &source.body.object_id, key)?;
    if bytes.len() as u64 != source.body.plaintext_byte_length.0 || segment::digest(&bytes) != hash
    {
        return Err(segment::corrupt());
    }
    Ok(Some(bytes))
}
fn promote(root: &Path, hash: &str, bytes: &[u8]) -> Result<Option<std::fs::File>> {
    let cas = crate::asset_repository::PayloadCas::new(root).map_err(local)?;
    let prepared = cas.prepare_bytes(bytes).map_err(local)?;
    if prepared.content_hash != hash {
        return Err(segment::corrupt());
    }
    crate::persistent_store::register_asset_objects_at_root(
        root,
        &[
            crate::persistent_store::asset_object_catalog::AssetObjectRegistration {
                object_hash: hash.into(),
                byte_size: bytes.len() as u64,
            },
        ],
        i64::try_from(super::runtime::now_ms()).map_err(local)?,
    )
    .map_err(local)?;
    cas.open_object(hash).map_err(local)
}
pub(crate) fn packed_body_groups(sources:Vec<PackedSource>,priority:&std::collections::BTreeSet<String>)->Vec<Vec<PackedSource>> {
    let mut parents=(0..sources.len()).collect::<Vec<_>>();
    let mut users=std::collections::BTreeMap::<String,usize>::new();
    fn root(parents:&mut [usize],mut index:usize)->usize {
        while parents[index]!=index {parents[index]=parents[parents[index]];index=parents[index];}index
    }
    for (index,source) in sources.iter().enumerate() {
        for chunk in &source.chunks {
            if let Some(previous)=users.insert(chunk.pack_id.clone(),index) {
                let left=root(&mut parents,index);let right=root(&mut parents,previous);
                parents[left.max(right)]=left.min(right);
            }
        }
    }
    let mut groups=std::collections::BTreeMap::<usize,Vec<PackedSource>>::new();
    for (index,source) in sources.into_iter().enumerate() {groups.entry(root(&mut parents,index)).or_default().push(source);}
    let mut groups=groups.into_values().collect::<Vec<_>>();
    groups.sort_by_key(|group|!group.iter().any(|source|priority.contains(&source.hash)));
    for group in &mut groups {group.sort_by_key(|source|!priority.contains(&source.hash));}
    groups
}
pub(crate) fn hydrate_registered_many(root:&Path,digests:&[String],priority:&std::collections::BTreeSet<String>,
    cancellation:Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,check:&dyn Fn()->crate::server_sync::Result<()>,
    opened:&mut dyn FnMut(&str,crate::server_sync::residency::HydrationOutcome))->crate::server_sync::Result<Vec<String>> {
    use crate::server_sync::residency::{with_hydration_lock,HydrationOutcome};
    fn error(_:impl std::fmt::Display)->crate::server_sync::SyncError {crate::server_sync::SyncError::new("external-body-unavailable",502)}
    fn remote_error(error:ProviderError)->crate::server_sync::SyncError {
        match error.kind {
            ErrorKind::Cancelled=>crate::server_sync::SyncError::new("cancelled",409),
            ErrorKind::ReauthRequired=>crate::server_sync::SyncError::new("external-reauth-required",502),
            _=>crate::server_sync::SyncError::new("external-body-unavailable",502),
        }
    }
    let installer_error=std::cell::RefCell::new(None);
    let cancel=cancellation.map(Cancellation::with_external_flag).unwrap_or_default();
    let cas=crate::asset_repository::PayloadCas::new(root)?;
    let scratch=tempfile::tempdir()?;
    let mut seen=std::collections::BTreeSet::new();
    let mut unavailable=Vec::new();
    let mut standalone=Vec::new();
    let mut connections=std::collections::BTreeMap::<(PathBuf,String,String),Vec<PackedSource>>::new();
    for hash in digests {
        check()?;cancel.check().map_err(remote_error)?;
        if !seen.insert(hash.clone()) {continue;}
        if cas.stat_object(hash)?.is_some() {opened(hash,HydrationOutcome::AlreadyLocal);continue;}
        match freeze_remote_body(root,hash).map_err(error)? {
            Some(FrozenBodySource::Standalone(source))=>standalone.push(source),
            Some(FrozenBodySource::Packed(source))=>connections.entry((source.connection_root.clone(),source.connection_id.clone(),source.library_id.clone())).or_default().push(source),
            None=>unavailable.push(hash.clone()),
        }
    }
    let mut groups=Vec::new();
    for (connection,sources) in connections {for group in packed_body_groups(sources,priority) {groups.push((connection.clone(),group));}}
    groups.sort_by_key(|(_,group)|!group.iter().any(|source|priority.contains(&source.hash)));
    standalone.sort_by_key(|source|!priority.contains(&source.hash));
    let mut install=|hash:&str,size:u64,path:&Path|->crate::server_sync::Result<()> {
        with_hydration_lock(root,hash,check,|| {
            check()?;cancel.check().map_err(remote_error)?;
            if let Some(actual)=cas.stat_object(hash)? {
                if actual!=size {return Err(error("invalid-body"));}
                opened(hash,HydrationOutcome::AlreadyLocal);return Ok(());
            }
            #[cfg(test)]
            let _scope=crate::asset_repository::body_io::object_scope(hash);
            let input=crate::trust_boundary::open_regular_source(path);
            #[cfg(test)]
            crate::asset_repository::body_io::open_result("managed",&input);
            let input=input?;
            #[cfg(test)]
            let mut input=crate::asset_repository::body_io::TrackedBodyFile::new(input,hash);
            #[cfg(not(test))]
            let mut input=input;
            let install_check=|| {check()?;cancel.check().map_err(remote_error)};
            let staged=crate::server_sync::transfer::stage_checked(&cas,&mut input,hash,size,&install_check)?;
            let _guard=crate::asset_repository::coordinator::lock_repository_mutation()?;
            check()?;cancel.check().map_err(remote_error)?;
            cas.publish_staged(staged)?;
            crate::persistent_store::register_asset_objects_at_root(root,&[crate::persistent_store::asset_object_catalog::AssetObjectRegistration{object_hash:hash.into(),byte_size:size}],i64::try_from(super::runtime::now_ms()).map_err(error)?).map_err(error)?;
            opened(hash,HydrationOutcome::Downloaded);Ok(())
        })
    };
    // Selected standalone bodies precede background pack groups.
    for source in standalone.iter().filter(|source|priority.contains(&source.hash)) {
        let frozen=FrozenBodySource::Standalone(source.clone());
        let file=tauri::async_runtime::block_on(spool_frozen_remote_body(&frozen,scratch.path(),&cancel)).map_err(remote_error)?;
        install(&source.hash,source.body.plaintext_byte_length.0,file.path())?;
    }
    for ((connection_root,connection_id,library_id),group) in groups {
        check()?;cancel.check().map_err(remote_error)?;
        let result=tauri::async_runtime::block_on(async {
            let (provider,repository,key)=open_source_connection(&connection_root,&connection_id,&library_id,&cancel).await?;
            let stored=ConnectionStore::open(&connection_root)?.read(&connection_id)?;
            for source in &group {validate_packed_source(source,&repository)?;}
            let writer=uuid::Uuid::new_v4().to_string();let job=uuid::Uuid::new_v4().to_string();
            let protection=super::leases::LeaseContext{root:&connection_root,connection_id:&connection_id,writer_id:&writer,descriptor:&stored.descriptor,root_key:&key,provider:provider.as_ref(),repository:&repository,clock:super::leases::system_clock(),protection_supported:stored.capabilities.lease_operations,ledger:None};
            let owner=match super::leases::admit_shared_work(&protection,&job,&cancel).await? {
                super::leases::Admission::Admitted(owner)=>owner,
                super::leases::Admission::Yield{reason}=>return Err(super::leases::yield_error(reason)),
                super::leases::Admission::UnsupportedProtection=>return Err(ProviderError::new(ErrorKind::Unsupported)),
            };
            owner.run(&protection,&cancel,async {
                let mut missing=Vec::new();
                for source in group {
                    if let Some(size)=cas.stat_object(&source.hash).map_err(local)? {if size!=source.byte_length {return Err(segment::corrupt());} install(&source.hash,size,scratch.path()).map_err(|error| {installer_error.replace(Some(error));segment::corrupt()})?;}
                    else {missing.push(source);}
                }
                if missing.is_empty() {return Ok(());}
                let stage=tempfile::tempdir_in(scratch.path()).map_err(local)?;
                let files=match super::snapshot_restore::download_packed_body_files(&missing,stage.path(),&key,provider.as_ref(),&repository,&cancel).await {
                    Err(error) if error.kind==ErrorKind::NotFound => {
                        let (current_repository,_)=provider.open_repository(&stored.config,&SecretRef(stored.credential_ref.clone()),OpenMode::Existing,&cancel).await?;
                        if current_repository.repository_id!=repository.repository_id || current_repository.connection_identity!=repository.connection_identity {return Err(segment::corrupt());}
                        let engine=super::lww_engine::ExternalLwwEngine {provider:provider.clone(),repository:current_repository,library:library_id.clone(),
                            root_key:zeroize::Zeroizing::new(*key),admission:None,connection_id:connection_id.clone(),connection_root:connection_root.clone(),
                            capabilities:stored.capabilities.clone(),descriptor:stored.descriptor.clone()};
                        let refreshed=engine.refresh_packed_sources(&missing,&scratch.path().join("current-catalogs"),&cancel).await?;
                        if let Some(reason)=owner.recheck(&protection,&cancel).await? {return Err(super::leases::yield_error(reason));}
                        check().map_err(|error| {installer_error.replace(Some(error));segment::corrupt()})?;
                        register_packed_many(root,&refreshed,&repository)?;
                        missing=refreshed;
                        super::snapshot_restore::download_packed_body_files(&missing,&stage.path().join("current-source"),&key,provider.as_ref(),&repository,&cancel).await?
                    }
                    other=>other?,
                };
                // Every read above finished before this check, so one check covers the whole group.
                if let Some(reason)=owner.recheck(&protection,&cancel).await? {return Err(super::leases::yield_error(reason));}
                for source in missing {
                    install(&source.hash,source.byte_length,files.get(&source.hash).ok_or_else(segment::corrupt)?).map_err(|error| {installer_error.replace(Some(error));segment::corrupt()})?;
                }
                Ok(())
            }).await
        });
        if let Some(error)=installer_error.borrow_mut().take() {return Err(error);}
        result.map_err(remote_error)?;
    }
    for source in standalone.iter().filter(|source|!priority.contains(&source.hash)) {
        let frozen=FrozenBodySource::Standalone(source.clone());
        let file=tauri::async_runtime::block_on(spool_frozen_remote_body(&frozen,scratch.path(),&cancel)).map_err(remote_error)?;
        install(&source.hash,source.body.plaintext_byte_length.0,file.path())?;
    }
    check()?;Ok(unavailable)
}
pub(crate) fn hydrate_registered(root:&Path,hash:&str,check:&dyn Fn()->crate::server_sync::Result<()>)->crate::server_sync::Result<bool> {
    Ok(hydrate_registered_many(root,&[hash.to_owned()],&std::collections::BTreeSet::new(),None,check,&mut |_,_| {})?.is_empty())
}
pub(crate) fn hydrate(
    root: &Path,
    hash: &str,
    check: &dyn Fn() -> crate::server_sync::Result<()>,
) -> crate::server_sync::Result<Option<std::fs::File>> {
    let cas = crate::asset_repository::PayloadCas::new(root)?;
    if let Some(file) = cas.open_object(hash)? {
        return Ok(Some(file));
    }
    if !hydrate_registered(root,hash,check)? {return Ok(None);}
    check()?;cas.open_object(hash).map_err(Into::into)
}

pub(crate) async fn hydrate_missing(
    root: &Path,
    hash: &str,
    cancel: &Cancellation,
) -> Result<Option<std::fs::File>> {
    let cas = crate::asset_repository::PayloadCas::new(root).map_err(local)?;
    if let Some(file) = cas.open_object(hash).map_err(local)? {
        return Ok(Some(file));
    }
    let Some((provider, repository, key)) = remote_source(root, hash, cancel).await? else {
        return Ok(None);
    };
    fulfill(root, hash, provider.as_ref(), &repository, &key, cancel).await
}
pub(crate) async fn spool_verified_remote_body(root:&Path,hash:&str,destination:&Path,cancel:&Cancellation)->Result<Option<tempfile::NamedTempFile>> {
    let Some(source)=freeze_remote_body(root,hash)? else {return Ok(None)};
    spool_frozen_remote_body(&source,destination,cancel).await.map(Some)
}
pub(crate) async fn spool_frozen_remote_body(source:&FrozenBodySource,destination:&Path,cancel:&Cancellation)->Result<tempfile::NamedTempFile> {
    let (hash,connection_root,connection_id,library_id)=match source {
        FrozenBodySource::Standalone(source)=>(&source.hash,&source.connection_root,&source.connection_id,&source.library_id),
        FrozenBodySource::Packed(source)=>(&source.hash,&source.connection_root,&source.connection_id,&source.library_id),
    };
    let (provider,repository,key)=open_source_connection(connection_root,connection_id,library_id,cancel).await?;
    let stored=ConnectionStore::open(connection_root)?.read(connection_id)?;
    if let FrozenBodySource::Packed(source)=source {validate_packed_source(source,&repository)?;}
    risunest_sync_wire::validate_hash(hash).map_err(local)?;
    let connection_root=connection_root.clone();
    let connection_id=connection_id.clone();
    let source=source.clone();
    let writer=uuid::Uuid::new_v4().to_string();
    let job=uuid::Uuid::new_v4().to_string();
    let protection=super::leases::LeaseContext{root:&connection_root,connection_id:&connection_id,writer_id:&writer,descriptor:&stored.descriptor,root_key:&key,provider:provider.as_ref(),repository:&repository,clock:super::leases::system_clock(),protection_supported:stored.capabilities.lease_operations,ledger:None};
    let owner=match super::leases::admit_shared_work(&protection,&job,cancel).await? {
        super::leases::Admission::Admitted(owner)=>owner,
        super::leases::Admission::Yield{reason}=>return Err(super::leases::yield_error(reason)),
        super::leases::Admission::UnsupportedProtection=>return Err(ProviderError::new(ErrorKind::Unsupported)),
    };
    owner.run(&protection,cancel,async {
        std::fs::create_dir_all(destination).map_err(local)?;
        let mut output=tempfile::NamedTempFile::new_in(destination).map_err(local)?;
        let scratch=tempfile::tempdir_in(destination).map_err(local)?;
        if let FrozenBodySource::Standalone(source)=&source {
            let locator=source.body.locator.as_ref().ok_or_else(segment::corrupt)?;
            locator.validate_for(&repository)?;
            let cipher=scratch.path().join("ciphertext");
            let mut sink=super::transfer::SpoolSink::create(&cipher,source.body.byte_length.0)?;
            let receipt=match provider.read_object(&repository,locator,None,&mut sink,cancel).await? {
                ReadReceipt::Body(receipt)=>receipt,
                ReadReceipt::NotModified(_)=>return Err(segment::corrupt()),
            };
            if receipt.locator!=*locator || receipt.byte_length!=source.body.byte_length.0 || !sink.is_verified() {return Err(segment::corrupt());}
            let _verified=super::transfer::SpoolSource::verified(&cipher,source.body.byte_length.0,&source.body.sha256)?;
            let mut input=crate::trust_boundary::open_regular_source(&cipher).map_err(local)?;
            segment::open_body_stream(&mut input,&mut output,&source.library_id,&source.body.object_id,&key,source.body.plaintext_byte_length.0)?;
            if !super::snapshot_restore::verify_body_file(output.path(),source.body.plaintext_byte_length.0,hash)? {return Err(segment::corrupt());}
        } else if let FrozenBodySource::Packed(source)=&source {
            let path=match super::snapshot_restore::download_packed_body_file(hash,source.byte_length,source.chunks.clone(),source.packs.clone(),scratch.path(),&key,provider.as_ref(),&repository,cancel).await {
                Err(error) if error.kind==ErrorKind::NotFound => {
                    let (current_repository,_)=provider.open_repository(&stored.config,&SecretRef(stored.credential_ref.clone()),OpenMode::Existing,cancel).await?;
                    if current_repository.repository_id!=repository.repository_id || current_repository.connection_identity!=repository.connection_identity {return Err(segment::corrupt());}
                    let engine=super::lww_engine::ExternalLwwEngine{provider:provider.clone(),repository:current_repository,library:source.library_id.clone(),
                        root_key:zeroize::Zeroizing::new(*key),admission:None,connection_id:connection_id.clone(),connection_root:connection_root.clone(),
                        capabilities:stored.capabilities.clone(),descriptor:stored.descriptor.clone()};
                    let mut refreshed=engine.refresh_packed_sources(std::slice::from_ref(source),&scratch.path().join("current-catalogs"),cancel).await?;
                    let refreshed=refreshed.pop().ok_or_else(segment::corrupt)?;
                    if let Some(reason)=owner.recheck(&protection,cancel).await? {return Err(super::leases::yield_error(reason));}
                    super::snapshot_restore::download_packed_body_file(hash,source.byte_length,refreshed.chunks,refreshed.packs,&scratch.path().join("current-source"),&key,provider.as_ref(),&repository,cancel).await?
                }
                other=>other?,
            };
            let mut input=crate::trust_boundary::open_regular_source(&path).map_err(local)?;
            std::io::copy(&mut input,&mut output).map_err(local)?;
        }
        output.as_file().sync_all().map_err(local)?;
        std::io::Seek::seek(output.as_file_mut(),std::io::SeekFrom::Start(0)).map_err(local)?;
        if let Some(reason)=owner.recheck(&protection,cancel).await? {return Err(super::leases::yield_error(reason));}
        Ok(output)
    }).await
}
pub(crate) async fn read_verified_remote_body(root:&Path,hash:&str,cancel:&Cancellation)->Result<Option<Vec<u8>>> {
    let temporary=tempfile::tempdir().map_err(local)?;
    let Some(source)=freeze_remote_body(root,hash)? else {return Ok(None)};
    let file=spool_frozen_remote_body(&source,temporary.path(),cancel).await?;
    read_frozen_body_spool(&source,&file,cancel).map(Some)
}
pub(crate) fn read_frozen_body_spool(source:&FrozenBodySource,file:&tempfile::NamedTempFile,cancel:&Cancellation)->Result<Vec<u8>> {
    use std::io::Read;
    cancel.check()?;
    let hash=match source {FrozenBodySource::Standalone(source)=>&source.hash,FrozenBodySource::Packed(source)=>&source.hash};
    risunest_sync_wire::validate_hash(hash).map_err(local)?;
    #[cfg(test)]
    let _scope=crate::asset_repository::body_io::object_scope(hash);
    let input=crate::trust_boundary::open_regular_source(file.path());
    #[cfg(test)]
    crate::asset_repository::body_io::open_result("managed",&input);
    let input=input.map_err(local)?;
    if input.metadata().map_err(local)?.len()!=source.byte_length() {return Err(segment::corrupt());}
    #[cfg(test)]
    let mut input=crate::asset_repository::body_io::TrackedBodyFile::new(input,hash);
    #[cfg(not(test))]
    let mut input=input;
    let mut bytes=Vec::new();
    bytes.try_reserve_exact(usize::try_from(source.byte_length()).map_err(local)?).map_err(local)?;
    let mut chunk=[0u8;64*1024];
    loop {
        cancel.check()?;
        let count=input.read(&mut chunk).map_err(local)?;
        if count==0 {break;}
        bytes.extend_from_slice(&chunk[..count]);
        if bytes.len() as u64>source.byte_length() {return Err(segment::corrupt());}
    }
    if bytes.len() as u64!=source.byte_length() {return Err(segment::corrupt());}
    cancel.check()?;
    Ok(bytes)
}
async fn fetch_missing(root:&Path,hash:&str,cancel:&Cancellation)->Result<Option<Vec<u8>>> {read_verified_remote_body(root,hash,cancel).await}
#[cfg(test)]
mod frozen_spool_tests {
    use super::*;
    use std::io::Write;
    #[test]
    fn frozen_spool_read_observes_actual_asset_open_and_bytes() {
        let bytes=b"synthetic captured remote body";
        let hash=risunest_sync_wire::hash(bytes);
        let source=FrozenBodySource::Standalone(Source {
            hash:hash.clone(),library_id:"synthetic-library".into(),connection_id:"synthetic-connection".into(),
            connection_root:PathBuf::new(),protected_segment:"synthetic-segment".into(),
            body:LargeBody {object_id:"synthetic-body".into(),sha256:hash.clone(),byte_length:(bytes.len() as u64).into(),plaintext_byte_length:(bytes.len() as u64).into(),locator:None},
        });
        let mut file=tempfile::NamedTempFile::new().unwrap();file.write_all(bytes).unwrap();
        crate::asset_repository::body_io::reset_body_io();
        crate::asset_repository::body_io::register_object_purpose(&hash,crate::asset_repository::body_io::BodyPurpose::Asset);
        assert_eq!(read_frozen_body_spool(&source,&file,&Cancellation::default()).unwrap(),bytes);
        let observed=crate::asset_repository::body_io::take_body_io();
        assert!(observed.complete());
        assert_eq!(observed.asset_work().opens,1);
        assert_eq!(observed.asset_work().read_bytes,bytes.len() as u64);
    }
}
#[cfg(test)]
mod registration_tests {
    use super::*;
    use risunest_external_storage_format::snapshot::{ObjectRole,PublicObjectHeader,StoredObject,WireLocator};
    #[test]
    fn batched_registration_records_every_source_across_batches() {
        let root=tempfile::tempdir().unwrap();
        register_verified_packed_many(root.path(),&[] as &[PackedSource]).unwrap();
        assert!(database(root.path(),false).unwrap().is_none(),"nothing to register creates no database");
        let catalog=StoredObject {
            header:PublicObjectHeader::new("synthetic-repository".into(),"synthetic-catalog".into(),ObjectRole::Catalog,1).unwrap(),
            locator:WireLocator {connection_identity:"synthetic-account".into(),collection:Some("catalogs".into()),object:"synthetic-object".into()},
            ciphertext_length:1,ciphertext_sha256:[1;32],plaintext_length:1,plaintext_sha256:[2;32],
        };
        let sources=(0..PACKED_REGISTRATION_BATCH*2+1).map(|index| PackedSource {
            hash:format!("{index:064x}"),byte_length:index as u64,library_id:"synthetic-library".into(),
            connection_id:"synthetic-connection".into(),connection_root:PathBuf::new(),protected_snapshot:"synthetic-snapshot".into(),
            catalog:catalog.clone(),chunks:Vec::new(),packs:Vec::new(),
        }).collect::<Vec<_>>();
        register_verified_packed_many(root.path(),&sources).unwrap();
        let db=database(root.path(),false).unwrap().unwrap();
        let count:i64=db.query_row("SELECT COUNT(*) FROM packed_sources",[],|row|row.get(0)).unwrap();
        assert_eq!(count,sources.len() as i64);
        for source in [&sources[0],&sources[PACKED_REGISTRATION_BATCH],&sources[sources.len()-1]] {
            assert_eq!(packed_source(root.path(),&source.hash).unwrap().unwrap().byte_length,source.byte_length);
        }
    }
}
async fn remote_source(
    root: &Path,
    hash: &str,
    cancel: &Cancellation,
) -> Result<
    Option<(
        std::sync::Arc<dyn Provider>,
        RepositoryHandle,
        zeroize::Zeroizing<[u8; 32]>,
    )>,
> {
    let (connection_root,connection_id,library_id)=if let Some(source)=source(root,hash)? {(source.connection_root,source.connection_id,source.library_id)} else if let Some(source)=packed_source(root,hash)? {(source.connection_root,source.connection_id,source.library_id)} else {return Ok(None)};
    open_source_connection(&connection_root,&connection_id,&library_id,cancel).await.map(Some)
}
#[cfg(test)]
fn test_source_connections()->&'static std::sync::Mutex<std::collections::BTreeMap<(std::path::PathBuf,String),std::sync::Arc<super::connection_commands::ConnectedRepository>>> {
    static CONNECTIONS:std::sync::OnceLock<std::sync::Mutex<std::collections::BTreeMap<(std::path::PathBuf,String),std::sync::Arc<super::connection_commands::ConnectedRepository>>>>=std::sync::OnceLock::new();
    CONNECTIONS.get_or_init(Default::default)
}
#[cfg(test)]
pub(crate) struct TestSourceConnection { key:(std::path::PathBuf,String) }
#[cfg(test)]
impl Drop for TestSourceConnection {
    fn drop(&mut self) {if let Ok(mut connections)=test_source_connections().lock() {connections.remove(&self.key);}}
}
#[cfg(test)]
pub(crate) fn install_test_source_connection(root:&Path,connection:std::sync::Arc<super::connection_commands::ConnectedRepository>)->Result<TestSourceConnection> {
    let key=(root.to_owned(),connection.stored.id.clone());
    let mut connections=test_source_connections().lock().map_err(super::runtime::local_error)?;
    if connections.contains_key(&key) {return Err(ProviderError::new(ErrorKind::PreconditionFailed));}
    connections.insert(key.clone(),connection);
    Ok(TestSourceConnection{key})
}
async fn open_source_connection(connection_root:&Path,connection_id:&str,library_id:&str,cancel:&Cancellation)->Result<(std::sync::Arc<dyn Provider>,RepositoryHandle,zeroize::Zeroizing<[u8;32]>)> {
    #[cfg(test)]
    {
        let connection=test_source_connections().lock().map_err(super::runtime::local_error)?.get(&(connection_root.to_owned(),connection_id.to_owned())).cloned();
        if let Some(connection)=connection {
            cancel.check()?;
            if connection.stored.descriptor.repository_id!=library_id {return Err(segment::corrupt());}
            let (repository,_)=connection.provider.open_repository(&connection.stored.config,&SecretRef(connection.stored.credential_ref.clone()),OpenMode::Existing,cancel).await?;
            if repository.repository_id!=connection.handle.repository_id || repository.connection_identity!=connection.handle.connection_identity {return Err(segment::corrupt());}
            return Ok((connection.provider.clone(),repository,zeroize::Zeroizing::new(*connection.root_key)));
        }
    }
    let stored = ConnectionStore::open(connection_root)?.read(connection_id)?;
    if stored.descriptor.repository_id != library_id {
        return Err(segment::corrupt());
    }
    let dependencies =
        super::connection::dependencies_for_config(&connection_root, &stored.config)?;
    let provider = super::connection::provider_for(&stored.config, dependencies)?;
    let (repository, _) = provider
        .open_repository(
            &stored.config,
            &SecretRef(stored.credential_ref.clone()),
            OpenMode::Existing,
            cancel,
        )
        .await?;
    let key = super::connection_commands::read_root_key(
        super::secrets::repository_key_vault(&connection_root).as_ref(),
        &stored.root_key_ref,
    )
    .await?;
    Ok((provider, repository, key))
}
