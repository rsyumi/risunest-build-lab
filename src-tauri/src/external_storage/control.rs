//! Authenticated mutable head and immutable history objects.
//!
//! Provider locators are opaque. Every control body therefore uses the shared
//! RNX1 envelope and repeats the descriptor repository identity inside the
//! authenticated plaintext. Public envelope fields are only routing hints
//! until the complete body has authenticated.
use super::{
    contract::{
        Cancellation, Collection, ErrorKind, HeadBytes, ObjectIntent, ObjectReceipt, ObjectRole,
        Provider, ProviderError, ReadReceipt, RemoteLocator, RepositoryHandle, Result,
    },
    journal::{SpoolAdmission, TransferJournal},
    packaging::{native_role, wire_role, RemoteObject},
    publication::HeadObservation,
    transfer_job,
};
use risunest_external_storage_format::{
    content_identity::hash,
    control as wire_control,
    crypto::derive_key,
    format::Descriptor,
    snapshot as wire,
};
#[cfg(test)]
use risunest_external_storage_format::format::Strategy;
use std::{
    fs,
    io::{Cursor, Read, Write},
};

#[cfg(test)]
pub(crate) fn observe_control_fingerprint(role:wire::ObjectRole,library:&[u8;32],sections:&std::collections::BTreeMap<String,wire::SectionSnapshotRef>,original:Option<&wire::StoredObject>,calls:usize) {
    use serde::Serialize;
    #[derive(Serialize)]
    #[serde(rename_all="camelCase")]
    struct Section<'a>{content:String,generation:&'a risunest_sync_wire::head::Sequence,gc_floor:&'a risunest_sync_wire::head::Sequence,max_write_clock:&'a risunest_sync_wire::head::Sequence}
    #[derive(Serialize)]
    struct State<'a>{domain:&'static str,library:String,sections:std::collections::BTreeMap<&'a str,Section<'a>>}
    #[derive(Serialize)]
    #[serde(rename_all="camelCase")]
    struct Bundle<'a>{#[serde(flatten)]state:State<'a>,original_units:Option<&'a wire::StoredObject>}
    struct Count(usize);
    impl std::io::Write for Count {
        fn write(&mut self,bytes:&[u8])->std::io::Result<usize>{self.0=self.0.checked_add(bytes.len()).ok_or_else(||std::io::Error::other("fingerprint input length overflow"))?;Ok(bytes.len())}
        fn flush(&mut self)->std::io::Result<()>{Ok(())}
    }
    let bundle=role==wire::ObjectRole::BackupBundle;
    let domain=if bundle {"native_external_bundle_fingerprint"} else {"native_external_state_fingerprint"};
    let state=State{domain:if bundle {wire::BUNDLE_FINGERPRINT_DOMAIN} else {wire::STATE_FINGERPRINT_DOMAIN},library:hex::encode(library),sections:sections.iter().map(|(id,section)|(id.as_str(),Section{content:hex::encode(section.content_fingerprint),generation:&section.generation,gc_floor:&section.gc_floor,max_write_clock:&section.max_write_clock})).collect()};
    let mut count=Count(0);
    let result=if bundle {serde_json::to_writer(&mut count,&Bundle{state,original_units:original})} else {serde_json::to_writer(&mut count,&state)};
    if result.is_err() {crate::persistent_store::hash_work::incomplete(domain);return;}
    for _ in 0..calls {crate::persistent_store::hash_work::observe(domain,count.0);}
}
#[cfg(test)]
pub(crate) fn observe_bundle_result(result:&risunest_external_storage_format::Result<wire_control::BackupBundleDocument>,calls:usize) {
    match result {Ok(document)=>observe_control_fingerprint(wire::ObjectRole::BackupBundle,&document.library.content_fingerprint,&document.sections,document.original_units.as_ref(),calls),Err(_)=>crate::persistent_store::hash_work::incomplete("native_external_bundle_fingerprint")}
}
#[cfg(test)]
pub(crate) fn observe_state_result(result:&risunest_external_storage_format::Result<wire::SyncStateDocument>,calls:usize) {
    match result {Ok(document)=>observe_control_fingerprint(wire::ObjectRole::SyncState,&document.library_fingerprint,&document.sections,None,calls),Err(_)=>crate::persistent_store::hash_work::incomplete("native_external_state_fingerprint")}
}

const HEAD_OBJECT_ID: &str = "head";
const MAX_CONTROL_PLAINTEXT: usize = 48 * 1024;
const MAX_POINT_PLAINTEXT: usize = wire_control::MAX_CONTROL_BYTES;
const MAX_POINT_CIPHERTEXT: u64 = 512 * 1024;
const MAX_SNAPSHOT_CIPHERTEXT: u64 = wire::MAX_METADATA_BYTES as u64 + 512 * 1024;
const MAX_DISCOVERY_PAGES: usize = 10_000;
const SNAPSHOT_DISCOVERY_PAGE_LIMIT: u16 = 100;

fn corrupt(error: impl std::fmt::Display) -> ProviderError {
    ProviderError::new(ErrorKind::Corrupt).caused(&error)
}
fn transient(error: impl std::fmt::Display) -> ProviderError {
    ProviderError::new(ErrorKind::Transient).caused(&error)
}
fn decode_hash(value: &str) -> Result<[u8; 32]> {
    hex::decode(value)
        .ok()
        .and_then(|bytes| bytes.try_into().ok())
        .filter(|_| crate::trust_boundary::is_lower_hex_256(value))
        .ok_or_else(|| corrupt("invalid hash"))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct HeadDocument {
    pub repository_id: String,
    pub library_id: String,
    pub commit_id: String,
    pub parent_commit_id: Option<String>,
    /// Identifies the whole published state, control changes included.
    pub content_fingerprint: String,
    pub state: RemoteObject,
}
impl HeadDocument {
    pub(crate) fn new(
        descriptor: &Descriptor,
        library_id: String,
        commit_id: String,
        parent_commit_id: Option<String>,
        content_fingerprint: String,
        state: RemoteObject,
    ) -> Result<Self> {
        let value = Self {
            repository_id: descriptor.repository_id.clone(),
            library_id,
            commit_id,
            parent_commit_id,
            content_fingerprint,
            state,
        };
        descriptor.validate().map_err(corrupt)?;
        value.check(descriptor)?;
        Ok(value)
    }
    fn check(&self, descriptor: &Descriptor) -> Result<()> {
        if self.repository_id != descriptor.repository_id
            || !crate::trust_boundary::is_lower_hex_256(&self.content_fingerprint)
            || self.state.repository_id != descriptor.repository_id
            || self.state.role != ObjectRole::SyncState
        {
            return Err(corrupt("invalid head document"));
        }
        Ok(())
    }
    fn to_wire(
        &self,
        descriptor: &Descriptor,
        repository: &RepositoryHandle,
    ) -> Result<wire_control::HeadDocument> {
        descriptor.validate().map_err(corrupt)?;
        self.check(descriptor)?;
        wire_control::HeadDocument::new(
            self.repository_id.clone(),
            self.library_id.clone(),
            self.commit_id.clone(),
            self.parent_commit_id.clone(),
            decode_hash(&self.content_fingerprint)?,
            self.state.stored(repository)?,
        )
        .map_err(corrupt)
    }
    fn from_wire(
        value: wire_control::HeadDocument,
        descriptor: &Descriptor,
        repository: &RepositoryHandle,
    ) -> Result<Self> {
        if value.repository_id != descriptor.repository_id {
            return Err(corrupt("head descriptor binding differs"));
        }
        Ok(Self {
            repository_id: value.repository_id,
            library_id: value.library_id,
            commit_id: value.commit_id,
            parent_commit_id: value.parent_commit_id,
            content_fingerprint: hex::encode(value.state_fingerprint),
            state: RemoteObject::from_stored(&value.state, repository)?,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BackupPointKind {
    Automatic,
    Manual,
    RecoveryCandidate,
}

impl BackupPointKind {
    fn to_wire(self) -> wire_control::BackupPointKind {
        match self {
            Self::Automatic => wire_control::BackupPointKind::Backup,
            Self::Manual => wire_control::BackupPointKind::Manual,
            Self::RecoveryCandidate => wire_control::BackupPointKind::History,
        }
    }
}

/// A point names the one remote bundle it preserves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BackupPointDocument {
    pub repository_id: String,
    pub point_id: String,
    pub kind: BackupPointKind,
    pub created_at_ms: u64,
    pub bundle: RemoteObject,
}
impl BackupPointDocument {
    pub(crate) fn single(
        descriptor: &Descriptor,
        point_id: String,
        kind: BackupPointKind,
        created_at_ms: u64,
        bundle: RemoteObject,
    ) -> Result<Self> {
        let value = Self {
            repository_id: descriptor.repository_id.clone(),
            point_id,
            kind,
            created_at_ms,
            bundle,
        };
        descriptor.validate().map_err(corrupt)?;
        value.check(descriptor)?;
        Ok(value)
    }
    pub(crate) fn bundles(&self) -> Vec<&RemoteObject> {
        vec![&self.bundle]
    }
    fn check(&self, descriptor: &Descriptor) -> Result<()> {
        if self.repository_id != descriptor.repository_id
            || self.bundle.repository_id != descriptor.repository_id
            || self.bundle.role != ObjectRole::BackupBundle
        {
            return Err(corrupt("invalid backup point"));
        }
        Ok(())
    }
    fn to_wire(
        &self,
        descriptor: &Descriptor,
        repository: &RepositoryHandle,
    ) -> Result<wire_control::BackupPointDocument> {
        descriptor.validate().map_err(corrupt)?;
        self.check(descriptor)?;
        wire_control::BackupPointDocument::single(
            self.repository_id.clone(),
            self.point_id.clone(),
            self.kind.to_wire(),
            self.created_at_ms,
            self.bundle.stored(repository)?,
        )
        .map_err(corrupt)
    }
    fn from_wire(
        value: wire_control::BackupPointDocument,
        descriptor: &Descriptor,
        repository: &RepositoryHandle,
    ) -> Result<Self> {
        if value.repository_id != descriptor.repository_id {
            return Err(corrupt("backup point descriptor binding differs"));
        }
        let kind = match value.kind {
            wire_control::BackupPointKind::Backup => BackupPointKind::Automatic,
            wire_control::BackupPointKind::History => BackupPointKind::RecoveryCandidate,
            wire_control::BackupPointKind::Manual => BackupPointKind::Manual,
        };
        Ok(Self {
            repository_id: value.repository_id,
            point_id: value.point_id,
            kind,
            created_at_ms: value.created_at_ms,
            bundle: RemoteObject::from_stored(&value.bundle, repository)?,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ObservedHead {
    pub document: HeadDocument,
    pub observation: HeadObservation,
}

#[cfg(test)]
pub(crate) struct PreparedHead {
    pub bytes: HeadBytes,
    pub authenticated_body_hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ListedBackupPoint {
    pub document: BackupPointDocument,
    pub reference: RemoteObject,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct BackupPointPage {
    pub points: Vec<ListedBackupPoint>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ListedInventoryPage {
    pub document: wire_control::InventoryPageDocument,
    pub reference: RemoteObject,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InventoryPagePage {
    pub pages: Vec<ListedInventoryPage>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RemoteBackupPointDeleteOutcome {
    Deleted,
    NotFound,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RemoteInventoryPageDeleteOutcome {
    Deleted,
    NotFound,
}

pub(crate) struct InventoryRegistration<'a> {
    pub intent: &'a ObjectIntent,
    pub plaintext_length: u64,
    pub plaintext_sha256: &'a str,
}

pub(crate) fn inventory_page_document(
    descriptor: &Descriptor,
    repository: &RepositoryHandle,
    operation_id: &str,
    page_id: &str,
    registrations: &[InventoryRegistration<'_>],
) -> Result<wire_control::InventoryPageDocument> {
    descriptor.validate().map_err(corrupt)?;
    let mut objects = registrations
        .iter()
        .map(|registration| {
            let intent = registration.intent;
            if intent.job_id != operation_id {
                return Err(corrupt("inventory operation differs"));
            }
            intent.validate(repository)?;
            wire_control::InventoryEntry::new(
                intent.object_id.clone(),
                wire_role(intent.role)?,
                intent.byte_length,
                decode_hash(&intent.sha256)?,
                registration.plaintext_length,
                decode_hash(registration.plaintext_sha256)?,
            )
            .map_err(corrupt)
        })
        .collect::<Result<Vec<_>>>()?;
    objects.sort_by(|left, right| left.object_id.cmp(&right.object_id));
    wire_control::InventoryPageDocument::new(
        descriptor.repository_id.clone(),
        operation_id.to_owned(),
        page_id.to_owned(),
        objects,
    )
    .map_err(corrupt)
}

fn seal(
    descriptor: &Descriptor,
    root_key: &[u8; 32],
    object_id: &str,
    role: wire::ObjectRole,
    plaintext: &[u8],
) -> Result<Vec<u8>> {
    descriptor.validate().map_err(corrupt)?;
    let header = wire::PublicObjectHeader::new(
        descriptor.repository_id.clone(),
        object_id.into(),
        role,
        plaintext.len() as u64,
    )
    .map_err(corrupt)?;
    let key = derive_key(root_key, &descriptor.repository_id, "metadata").map_err(corrupt)?;
    let mut output = Vec::new();
    wire::seal_envelope(&mut Cursor::new(plaintext), &mut output, &key, &header)
        .map_err(corrupt)?;
    Ok(output)
}

fn open(
    descriptor: &Descriptor,
    root_key: &[u8; 32],
    expected_id: Option<&str>,
    expected_role: wire::ObjectRole,
    ciphertext: &[u8],
    max_plaintext: usize,
) -> Result<(Vec<u8>, wire::PublicObjectHeader, String, String)> {
    descriptor.validate().map_err(corrupt)?;
    let key = derive_key(root_key, &descriptor.repository_id, "metadata").map_err(corrupt)?;
    let mut plaintext = Vec::new();
    let header = wire::open_envelope(
        &mut Cursor::new(ciphertext),
        &mut plaintext,
        &key,
        max_plaintext as u64,
    )
    .map_err(corrupt)?;
    if header.repository_id != descriptor.repository_id
        || header.role != expected_role
        || expected_id.is_some_and(|id| header.object_id != id)
    {
        return Err(corrupt("control envelope binding differs"));
    }
    let plaintext_hash = hex::encode(hash(&plaintext));
    let ciphertext_hash = hex::encode(hash(ciphertext));
    Ok((plaintext, header, plaintext_hash, ciphertext_hash))
}

#[cfg(test)]
pub(crate) fn prepare_head(
    descriptor: &Descriptor,
    root_key: &[u8; 32],
    repository: &RepositoryHandle,
    document: HeadDocument,
) -> Result<PreparedHead> {
    let plaintext = document
        .to_wire(descriptor, repository)?
        .encode(MAX_CONTROL_PLAINTEXT)
        .map_err(corrupt)?;
    let sealed = seal(
        descriptor,
        root_key,
        HEAD_OBJECT_ID,
        wire::ObjectRole::Head,
        &plaintext,
    )?;
    let authenticated_body_hash = hex::encode(hash(&sealed));
    Ok(PreparedHead {
        bytes: HeadBytes::new(sealed)?,
        authenticated_body_hash,
    })
}

/// Control objects are small and bounded, so they are received in memory.
struct ControlSink {
    bytes: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    max_length: u64,
    verified: bool,
}
impl ControlSink {
    fn new(max_length: u64) -> Self {
        Self { bytes: Default::default(), max_length, verified: false }
    }
    fn take(&self) -> Result<Vec<u8>> {
        Ok(std::mem::take(&mut *self.bytes.lock().map_err(transient)?))
    }
}
/// The writer refuses a provider overrun before it reaches memory.
struct ControlWriter {
    bytes: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    remaining: u64,
    cancel: Cancellation,
}
impl tokio::io::AsyncWrite for ControlWriter {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        bytes: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        if self.cancel.check().is_err() {
            return std::task::Poll::Ready(Err(std::io::Error::other("cancelled")));
        }
        if bytes.len() as u64 > self.remaining {
            return std::task::Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "transfer-limit-exceeded",
            )));
        }
        match self.bytes.lock() {
            Ok(mut out) => out.extend_from_slice(bytes),
            Err(_) => return std::task::Poll::Ready(Err(std::io::Error::other("control receive lock"))),
        }
        self.remaining -= bytes.len() as u64;
        std::task::Poll::Ready(Ok(bytes.len()))
    }
    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}
impl super::contract::TransferSink for ControlSink {
    fn open<'a>(
        &'a mut self,
        offset: u64,
        max_length: u64,
        cancel: &'a Cancellation,
    ) -> super::contract::ProviderFuture<'a, std::pin::Pin<Box<dyn tokio::io::AsyncWrite + Send>>> {
        Box::pin(async move {
            cancel.check()?;
            let mut bytes = self.bytes.lock().map_err(transient)?;
            if self.verified
                || offset.checked_add(max_length).is_none_or(|end| end > self.max_length)
                || offset > bytes.len() as u64
            {
                return Err(ProviderError::new(ErrorKind::Corrupt));
            }
            bytes.truncate(offset as usize);
            drop(bytes);
            Ok(Box::pin(ControlWriter {
                bytes: self.bytes.clone(),
                remaining: max_length,
                cancel: cancel.clone(),
            }) as std::pin::Pin<Box<dyn tokio::io::AsyncWrite + Send>>)
        })
    }
    fn finish<'a>(&'a mut self, length: u64, sha256: &'a str) -> super::contract::ProviderFuture<'a, ()> {
        Box::pin(async move {
            let bytes = self.bytes.lock().map_err(transient)?;
            if self.verified
                || length > self.max_length
                || bytes.len() as u64 != length
                || hex::encode(hash(&bytes)) != sha256
            {
                return Err(ProviderError::new(ErrorKind::Corrupt));
            }
            drop(bytes);
            self.verified = true;
            Ok(())
        })
    }
}

async fn download_control(
    provider: &dyn Provider,
    repository: &RepositoryHandle,
    locator: &RemoteLocator,
    unchanged: Option<&super::contract::VersionToken>,
    max_ciphertext: u64,
    cancel: &Cancellation,
) -> Result<(ReadReceipt, Option<Vec<u8>>)> {
    let mut sink = ControlSink::new(max_ciphertext);
    let receipt = provider
        .read_object(repository, locator, unchanged, &mut sink, cancel)
        .await?;
    match &receipt {
        ReadReceipt::NotModified(_) => Ok((receipt, None)),
        ReadReceipt::Body(body) => {
            if !body.complete || !sink.verified || body.byte_length > max_ciphertext {
                return Err(corrupt("incomplete control object"));
            }
            let bytes = sink.take()?;
            if bytes.len() as u64 != body.byte_length {
                return Err(corrupt("control object length differs"));
            }
            Ok((receipt, Some(bytes)))
        }
    }
}

pub(crate) async fn read_head(
    provider: &dyn Provider,
    repository: &RepositoryHandle,
    descriptor: &Descriptor,
    root_key: &[u8; 32],
    known: Option<&ObservedHead>,
    cancel: &Cancellation,
) -> Result<Option<ObservedHead>> {
    let locator = provider.head_locator(repository)?;
    let unchanged = known.and_then(|head| head.observation.version.as_ref());
    let response = download_control(
        provider,
        repository,
        &locator,
        unchanged,
        HeadBytes::MAX_BYTES as u64,
        cancel,
    )
    .await;
    let (receipt, bytes) = match response {
        Err(error) if error.kind == ErrorKind::NotFound => return Ok(None),
        other => other?,
    };
    match (receipt, bytes) {
        (ReadReceipt::NotModified(version), None) => {
            let known = known
                .cloned()
                .ok_or_else(|| corrupt("not-modified without known head"))?;
            if known.observation.version.as_ref() != Some(&version) {
                return Err(corrupt("head version changed in not-modified response"));
            }
            Ok(Some(known))
        }
        (ReadReceipt::Body(receipt), Some(bytes)) => {
            let (plaintext, _, _, ciphertext_hash) = open(
                descriptor,
                root_key,
                Some(HEAD_OBJECT_ID),
                wire::ObjectRole::Head,
                &bytes,
                MAX_CONTROL_PLAINTEXT,
            )?;
            let wire = wire_control::HeadDocument::decode(&plaintext, MAX_CONTROL_PLAINTEXT)
                .map_err(corrupt)?;
            let document = HeadDocument::from_wire(wire, descriptor, repository)?;
            Ok(Some(ObservedHead {
                observation: HeadObservation {
                    commit_id: document.commit_id.clone(),
                    authenticated_body_hash: ciphertext_hash,
                    version: receipt.version,
                },
                document,
            }))
        }
        _ => Err(corrupt("invalid control download state")),
    }
}

pub(crate) async fn upload_backup_point(
    descriptor: &Descriptor,
    root_key: &[u8; 32],
    document: BackupPointDocument,
    journal: &mut TransferJournal,
    provider: &dyn Provider,
    repository: &RepositoryHandle,
    cancel: &Cancellation,
) -> Result<RemoteObject> {
    let wire_document = document.to_wire(descriptor, repository)?;
    let plaintext = wire_document.encode(MAX_POINT_PLAINTEXT).map_err(corrupt)?;
    upload_control_object(
        descriptor,
        root_key,
        format!("backup-point-{}", document.point_id),
        ObjectRole::BackupPoint,
        &plaintext,
        journal,
        provider,
        repository,
        cancel,
    )
    .await
}

/// Spool bytes the sealed inventory page registering `members` objects takes
/// at most, for the object and job identities this app names. A wave admits
/// each member with this room, so the page that registers the wave fits.
pub(crate) fn inventory_page_headroom(members: usize) -> u64 {
    const PAGE: u64 = 2 * 1024;
    const ENTRY: u64 = 512;
    PAGE.saturating_add(ENTRY.saturating_mul(members as u64))
}

pub(crate) async fn upload_inventory_page(
    descriptor: &Descriptor,
    root_key: &[u8; 32],
    document: wire_control::InventoryPageDocument,
    journal: &mut TransferJournal,
    provider: &dyn Provider,
    repository: &RepositoryHandle,
    cancel: &Cancellation,
) -> Result<RemoteObject> {
    if document.repository_id != descriptor.repository_id {
        return Err(corrupt("inventory repository differs"));
    }
    let plaintext = document
        .encode(MAX_POINT_PLAINTEXT)
        .map_err(corrupt)?;
    upload_control_object(
        descriptor,
        root_key,
        format!("inventory-page-{}", document.page_id),
        ObjectRole::InventoryPage,
        &plaintext,
        journal,
        provider,
        repository,
        cancel,
    )
    .await
}

/// Wraps an already published library reference in its own immutable bundle so
/// a retained point names a bundle rather than a synchronized state. A state is
/// the merged result of several devices, which is why the source says so. The
/// sections it carried travel with it, or the retained point would name a
/// library without the device data that state published.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn upload_backup_bundle(
    descriptor: &Descriptor,
    root_key: &[u8; 32],
    bundle_id: String,
    source: wire_control::BundleSource,
    captured_at_ms: u64,
    library: wire::LibrarySnapshotRef,
    sections: std::collections::BTreeMap<String, wire::SectionSnapshotRef>,
    original_units: Option<wire::StoredObject>,
    journal: &mut TransferJournal,
    provider: &dyn Provider,
    repository: &RepositoryHandle,
    cancel: &Cancellation,
) -> Result<RemoteObject> {
    let document = wire_control::BackupBundleDocument::new(
        descriptor.repository_id.clone(),
        bundle_id.clone(),
        source,
        captured_at_ms,
        None,
        None,
        None,
        library,
        sections,
        original_units,
    );
    #[cfg(test)] observe_bundle_result(&document,2);
    let document=document.map_err(corrupt)?;
    let plaintext = document.encode(MAX_POINT_PLAINTEXT).map_err(corrupt)?;
    upload_control_object(
        descriptor,
        root_key,
        format!("snapshot-{bundle_id}"),
        ObjectRole::BackupBundle,
        &plaintext,
        journal,
        provider,
        repository,
        cancel,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn upload_control_object(
    descriptor: &Descriptor,
    root_key: &[u8; 32],
    object_id: String,
    role: ObjectRole,
    plaintext: &[u8],
    journal: &mut TransferJournal,
    provider: &dyn Provider,
    repository: &RepositoryHandle,
    cancel: &Cancellation,
) -> Result<RemoteObject> {
    let _spool_execution = journal.begin_spool_execution();
    let plaintext_sha256 = hex::encode(hash(plaintext));
    let spool = journal.spool_path(&object_id);
    if journal.record(&object_id)?.is_none() {
        let ciphertext = seal(
            descriptor,
            root_key,
            &object_id,
            wire_role(role)?,
            plaintext,
        )?;
        if ciphertext.len() as u64 > MAX_POINT_CIPHERTEXT {
            return Err(ProviderError::new(ErrorKind::FileTooLarge));
        }
        // A file here that the journal never registered is a write that
        // stopped before it was, so it is replaced rather than charged again.
        match fs::symlink_metadata(&spool) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(transient(error)),
            Ok(metadata) if crate::trust_boundary::is_link_like(&metadata) => {
                return Err(corrupt("control spool is a link"));
            }
            Ok(_) => fs::remove_file(&spool).map_err(transient)?,
        }
        // Charged to the transfer spool before the file exists, with room for
        // the page that registers it. A page is what sends the wave it
        // registers, so it is charged without ever being refused.
        let _room = if role == ObjectRole::InventoryPage {
            journal.charge_page(ciphertext.len() as u64)?
        } else {
            match journal.reserve_spool_after_release(
                (ciphertext.len() as u64).saturating_add(inventory_page_headroom(1)),
                cancel,
            ).await? {
                SpoolAdmission::Admitted(room) => room,
                SpoolAdmission::Full => return Err(ProviderError::new(ErrorKind::Transient)),
            }
        };
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&spool)
            .map_err(transient)?;
        file.write_all(&ciphertext).map_err(transient)?;
        file.sync_all().map_err(transient)?;
        drop(file);
        let intent = ObjectIntent {
            repository_id: repository.repository_id.clone(),
            job_id: journal.job_id().into(),
            object_id: object_id.clone(),
            role,
            byte_length: ciphertext.len() as u64,
            sha256: hex::encode(hash(&ciphertext)),
        };
        journal.register(&intent)?;
    }
    let record = journal
        .record(&object_id)?
        .ok_or_else(|| corrupt("missing control journal"))?;
    if record.intent.role != role {
        return Err(corrupt("history journal role differs"));
    }
    // A released object left no local bytes to compare. Every role the
    // inventory carries was registered with its plaintext identity, which says
    // the same thing, and a page is named by the members it covers.
    if record.released {
        if role != ObjectRole::InventoryPage {
            let Some((length, digest)) = &record.plaintext else {
                return Err(corrupt("released control object was never registered"));
            };
            if *length != plaintext.len() as u64 || digest != &plaintext_sha256 {
                return Err(corrupt("control journal content differs"));
            }
        }
    } else {
        let mut recorded = crate::trust_boundary::open_regular_source(&spool).map_err(corrupt)?;
        let mut recorded_bytes = Vec::new();
        recorded.read_to_end(&mut recorded_bytes).map_err(corrupt)?;
        if recorded_bytes.len() as u64 != record.intent.byte_length
            || hex::encode(hash(&recorded_bytes)) != record.intent.sha256
        {
            return Err(corrupt("control journal bytes differ"));
        }
        let (recorded_plaintext, _, _, _) = open(
            descriptor,
            root_key,
            Some(&object_id),
            wire_role(role)?,
            &recorded_bytes,
            MAX_POINT_PLAINTEXT,
        )?;
        if recorded_plaintext != plaintext {
            return Err(corrupt("control journal content differs"));
        }
    }
    let receipt = if role == ObjectRole::InventoryPage {
        transfer_job::upload_inventory(journal, &object_id, provider, repository, cancel).await?
    } else {
        transfer_job::upload_registered(
            journal,
            &object_id,
            &descriptor.repository_id,
            root_key,
            plaintext.len() as u64,
            &plaintext_sha256,
            provider,
            repository,
            cancel,
        )
        .await?
    };
    Ok(RemoteObject {
        repository_id: descriptor.repository_id.clone(),
        object_id,
        role,
        receipt,
        ciphertext_sha256: record.intent.sha256,
        plaintext_length: plaintext.len() as u64,
        plaintext_sha256,
    })
}

async fn open_listed_point(
    receipt: ObjectReceipt,
    descriptor: &Descriptor,
    root_key: &[u8; 32],
    provider: &dyn Provider,
    repository: &RepositoryHandle,
    cancel: &Cancellation,
) -> Result<ListedBackupPoint> {
    receipt.locator.validate_for(repository)?;
    if !receipt.complete || receipt.byte_length == 0 || receipt.byte_length > MAX_POINT_CIPHERTEXT {
        return Err(corrupt("invalid listed history object"));
    }
    let (_, bytes) = download_control(
        provider,
        repository,
        &receipt.locator,
        None,
        MAX_POINT_CIPHERTEXT,
        cancel,
    )
    .await?;
    let bytes = bytes.ok_or_else(|| corrupt("listed point was not downloaded"))?;
    let (plaintext, header, plaintext_sha256, ciphertext_sha256) = open(
        descriptor,
        root_key,
        None,
        wire::ObjectRole::BackupPoint,
        &bytes,
        MAX_POINT_PLAINTEXT,
    )?;
    let wire_document = wire_control::BackupPointDocument::decode(&plaintext, MAX_POINT_PLAINTEXT)
        .map_err(corrupt)?;
    let document = BackupPointDocument::from_wire(wire_document, descriptor, repository)?;
    if header.object_id != format!("backup-point-{}", document.point_id) {
        return Err(corrupt("history object identity differs"));
    }
    Ok(ListedBackupPoint {
        reference: RemoteObject {
            repository_id: descriptor.repository_id.clone(),
            object_id: header.object_id,
            role: ObjectRole::BackupPoint,
            receipt,
            ciphertext_sha256,
            plaintext_length: header.plaintext_length,
            plaintext_sha256,
        },
        document,
    })
}

async fn open_listed_inventory_page(
    receipt: ObjectReceipt,
    descriptor: &Descriptor,
    root_key: &[u8; 32],
    provider: &dyn Provider,
    repository: &RepositoryHandle,
    cancel: &Cancellation,
) -> Result<ListedInventoryPage> {
    open_listed_inventory_page_in_scope(receipt,descriptor,root_key,provider,repository,cancel,None).await
}
async fn open_listed_inventory_page_in_scope(
    receipt: ObjectReceipt,
    descriptor: &Descriptor,
    root_key: &[u8; 32],
    provider: &dyn Provider,
    repository: &RepositoryHandle,
    cancel: &Cancellation,
    extra_repository_id:Option<&str>,
) -> Result<ListedInventoryPage> {
    receipt.locator.validate_for(repository)?;
    if !receipt.complete || receipt.byte_length == 0 || receipt.byte_length > MAX_POINT_CIPHERTEXT {
        return Err(corrupt("invalid listed inventory page"));
    }
    let (_, bytes) = download_control(
        provider,
        repository,
        &receipt.locator,
        None,
        MAX_POINT_CIPHERTEXT,
        cancel,
    )
    .await?;
    let bytes = bytes.ok_or_else(|| corrupt("listed inventory page was not downloaded"))?;
    let (advertised,_)=wire::read_public_header(&mut Cursor::new(&bytes)).map_err(corrupt)?;
    if advertised.role!=wire::ObjectRole::InventoryPage || (advertised.repository_id!=descriptor.repository_id
        && extra_repository_id!=Some(advertised.repository_id.as_str())) {return Err(corrupt("inventory page repository differs"))}
    let mut scoped=descriptor.clone();scoped.repository_id=advertised.repository_id;
    let descriptor=&scoped;
    let (plaintext, header, plaintext_sha256, ciphertext_sha256) = open(
        descriptor,
        root_key,
        None,
        wire::ObjectRole::InventoryPage,
        &bytes,
        MAX_POINT_PLAINTEXT,
    )?;
    let document = wire_control::InventoryPageDocument::decode(&plaintext, MAX_POINT_PLAINTEXT)
        .map_err(corrupt)?;
    if document.repository_id != descriptor.repository_id
        || header.object_id != format!("inventory-page-{}", document.page_id)
    {
        return Err(corrupt("inventory page identity differs"));
    }
    Ok(ListedInventoryPage {
        reference: RemoteObject {
            repository_id: descriptor.repository_id.clone(),
            object_id: header.object_id,
            role: ObjectRole::InventoryPage,
            receipt,
            ciphertext_sha256,
            plaintext_length: header.plaintext_length,
            plaintext_sha256,
        },
        document,
    })
}

pub(crate) async fn delete_authenticated_inventory_page(
    connected: &super::connection_commands::ConnectedRepository,
    expected: &wire::StoredObject,
    cancel: &Cancellation,
) -> Result<RemoteInventoryPageDeleteOutcome> {
    delete_authenticated_inventory_page_for_repository(connected,expected,&connected.stored.descriptor.repository_id,cancel).await
}
pub(crate) async fn delete_authenticated_inventory_page_for_repository(
    connected:&super::connection_commands::ConnectedRepository,expected:&wire::StoredObject,repository_id:&str,cancel:&Cancellation,
)->Result<RemoteInventoryPageDeleteOutcome> {
    if expected.header.repository_id!=repository_id {return Err(corrupt("inventory page repository differs"))}
    let mut descriptor=connected.stored.descriptor.clone();descriptor.repository_id=repository_id.into();
    if expected.header.role != wire::ObjectRole::InventoryPage
        || !expected.header.object_id.starts_with("inventory-page-")
    {
        return Err(corrupt("inventory page reference differs"));
    }
    let expected = RemoteObject::from_stored(expected, &connected.handle)?;
    let listed = match open_listed_inventory_page(
        expected.receipt.clone(),
        &descriptor,
        &connected.root_key,
        connected.provider.as_ref(),
        &connected.handle,
        cancel,
    )
    .await
    {
        Err(error) if error.kind == ErrorKind::NotFound => {
            return Ok(RemoteInventoryPageDeleteOutcome::NotFound)
        }
        other => other?,
    };
    if listed.reference.stored(&connected.handle)? != expected.stored(&connected.handle)? {
        return Err(corrupt("inventory page bytes differ"));
    }
    connected
        .provider
        .delete_object(&connected.handle, &expected.receipt.locator, cancel)
        .await?;
    Ok(RemoteInventoryPageDeleteOutcome::Deleted)
}

pub(crate) async fn list_backup_points_page(
    descriptor: &Descriptor,
    root_key: &[u8; 32],
    provider: &dyn Provider,
    repository: &RepositoryHandle,
    cursor: Option<&str>,
    limit: u16,
    cancel: &Cancellation,
) -> Result<BackupPointPage> {
    if limit == 0 || limit > 100 {
        return Err(corrupt("invalid history page limit"));
    }
    let page = provider
        .list_objects(repository, Collection::BackupPoints, cursor, limit, cancel)
        .await?;
    let mut points = Vec::with_capacity(page.objects.len());
    for receipt in page.objects {
        cancel.check()?;
        points.push(
            open_listed_point(receipt, descriptor, root_key, provider, repository, cancel).await?,
        );
    }
    Ok(BackupPointPage {
        points,
        next_cursor: page.next_cursor,
    })
}

pub(crate) async fn delete_authenticated_backup_point(
    connected: &super::connection_commands::ConnectedRepository,
    point_id: &str,
    expected_kind: BackupPointKind,
    point: &wire::StoredObject,
    cancel: &Cancellation,
) -> Result<RemoteBackupPointDeleteOutcome> {
    if point_id.is_empty()
        || point_id.len() > 1024
        || point_id.contains('\0')
        || !matches!(expected_kind, BackupPointKind::Automatic | BackupPointKind::Manual)
        || point.header.role != wire::ObjectRole::BackupPoint
        || point.header.object_id != format!("backup-point-{point_id}")
    {
        return Err(ProviderError::new(ErrorKind::PreconditionFailed));
    }
    let expected = RemoteObject::from_stored(point, &connected.handle)?;
    let listed = match open_listed_point(
        expected.receipt.clone(),
        &connected.stored.descriptor,
        &connected.root_key,
        connected.provider.as_ref(),
        &connected.handle,
        cancel,
    )
    .await
    {
        Err(error) if error.kind == ErrorKind::NotFound => {
            return Ok(RemoteBackupPointDeleteOutcome::NotFound)
        }
        other => other?,
    };
    if listed.document.point_id != point_id
        || listed.document.kind != expected_kind
        || listed.reference.stored(&connected.handle)? != *point
    {
        return Err(ProviderError::new(ErrorKind::PreconditionFailed));
    }
    connected
        .provider
        .delete_object(&connected.handle, &expected.receipt.locator, cancel)
        .await?;
    Ok(RemoteBackupPointDeleteOutcome::Deleted)
}

#[cfg(test)]
pub(crate) async fn list_inventory_pages_page(
    descriptor: &Descriptor,
    root_key: &[u8; 32],
    provider: &dyn Provider,
    repository: &RepositoryHandle,
    cursor: Option<&str>,
    limit: u16,
    cancel: &Cancellation,
) -> Result<InventoryPagePage> {
    list_inventory_pages_page_in_scope(descriptor,root_key,provider,repository,cursor,limit,cancel,None).await
}
pub(crate) async fn list_inventory_pages_page_in_scope(
    descriptor:&Descriptor,root_key:&[u8;32],provider:&dyn Provider,repository:&RepositoryHandle,
    cursor:Option<&str>,limit:u16,cancel:&Cancellation,extra_repository_id:Option<&str>,
)->Result<InventoryPagePage> {
    if limit == 0 || limit > 100 {
        return Err(corrupt("invalid inventory page limit"));
    }
    let page = provider
        .list_objects(
            repository,
            Collection::InventoryPages,
            cursor,
            limit,
            cancel,
        )
        .await?;
    let mut pages = Vec::with_capacity(page.objects.len());
    for receipt in page.objects {
        cancel.check()?;
        pages.push(
            open_listed_inventory_page_in_scope(
                receipt,
                descriptor,
                root_key,
                provider,
                repository,
                cancel,
                extra_repository_id,
            )
            .await?,
        );
    }
    Ok(InventoryPagePage {
        pages,
        next_cursor: page.next_cursor,
    })
}

/// Opens one enumerated state or bundle. Its identity comes from the
/// authenticated envelope, because a listing only carries routing hints.
pub(crate) async fn open_listed_snapshot(
    connected: &super::connection_commands::ConnectedRepository,
    receipt: ObjectReceipt,
    cancel: &Cancellation,
) -> Result<(RemoteObject, SnapshotView)> {
    if !receipt.complete || receipt.byte_length == 0 || receipt.byte_length > MAX_SNAPSHOT_CIPHERTEXT
    {
        return Err(corrupt("invalid listed snapshot object"));
    }
    let (read, bytes) = download_control(
        connected.provider.as_ref(),
        &connected.handle,
        &receipt.locator,
        None,
        MAX_SNAPSHOT_CIPHERTEXT,
        cancel,
    )
    .await?;
    let ReadReceipt::Body(read_receipt) = read else {
        return Err(corrupt("listed snapshot was not downloaded"));
    };
    if read_receipt.locator != receipt.locator
        || read_receipt.byte_length != receipt.byte_length
        || !read_receipt.complete
    {
        return Err(corrupt("listed snapshot receipt differs"));
    }
    let bytes = bytes.ok_or_else(|| corrupt("listed snapshot was not downloaded"))?;
    let repository_id = &connected.stored.descriptor.repository_id;
    let key = derive_key(&connected.root_key, repository_id, "metadata").map_err(corrupt)?;
    let mut plaintext = Vec::new();
    let header = wire::open_envelope(
        &mut Cursor::new(&bytes),
        &mut plaintext,
        &key,
        wire::MAX_METADATA_BYTES as u64,
    )
    .map_err(corrupt)?;
    if header.repository_id != *repository_id {
        return Err(corrupt("snapshot discovery envelope differs"));
    }
    let view = SnapshotView::read(&plaintext, header.role, repository_id)?;
    if header.object_id != format!("snapshot-{}", view.snapshot_id) {
        return Err(corrupt("snapshot discovery document differs"));
    }
    Ok((
        RemoteObject {
            repository_id: repository_id.clone(),
            object_id: header.object_id,
            role: native_role(header.role)?,
            receipt: read_receipt,
            ciphertext_sha256: hex::encode(hash(&bytes)),
            plaintext_length: header.plaintext_length,
            plaintext_sha256: hex::encode(hash(&plaintext)),
        },
        view,
    ))
}

/// Everything one catalog node names: the nodes below it and the packs it
/// holds. A restore only needs the packs, but nothing enumerates an
/// intermediate node, so a cleanup has to see both.
pub(crate) async fn read_catalog_children(
    connected: &super::connection_commands::ConnectedRepository,
    node: &RemoteObject,
    cancel: &Cancellation,
) -> Result<Vec<RemoteObject>> {
    read_catalog_children_for_repository(connected,node,&connected.stored.descriptor.repository_id,cancel).await
}
pub(crate) async fn read_catalog_children_for_repository(
    connected:&super::connection_commands::ConnectedRepository,node:&RemoteObject,repository_id:&str,cancel:&Cancellation,
)->Result<Vec<RemoteObject>> {
    let mut descriptor=connected.stored.descriptor.clone();descriptor.repository_id=repository_id.into();
    if node.role != ObjectRole::Catalog || node.repository_id != repository_id {
        return Err(corrupt("invalid catalog node"));
    }
    node.stored(&connected.handle)?;
    let (_, bytes) = download_control(
        connected.provider.as_ref(),
        &connected.handle,
        &node.receipt.locator,
        None,
        MAX_SNAPSHOT_CIPHERTEXT,
        cancel,
    )
    .await?;
    let bytes = bytes.ok_or_else(|| corrupt("catalog node was not downloaded"))?;
    let (plaintext, _, plaintext_sha256, ciphertext_sha256) = open(
        &descriptor,
        &connected.root_key,
        Some(&node.object_id),
        wire::ObjectRole::Catalog,
        &bytes,
        wire::MAX_METADATA_BYTES,
    )?;
    if plaintext_sha256 != node.plaintext_sha256 || ciphertext_sha256 != node.ciphertext_sha256 {
        return Err(corrupt("catalog node differs"));
    }
    let document = wire::CatalogDocument::decode(&plaintext, wire::MAX_METADATA_BYTES)
        .map_err(corrupt)?;
    let mut objects = Vec::new();
    for child in &document.children {
        objects.push(RemoteObject::from_stored(&child.object, &connected.handle)?);
    }
    for pack in &document.packs {
        objects.push(RemoteObject::from_stored(pack, &connected.handle)?);
    }
    Ok(objects)
}

async fn scan_snapshot(
    connected: &super::connection_commands::ConnectedRepository,
    snapshot_id: &str,
    cancel: &Cancellation,
) -> Result<RemoteObject> {
    let expected_object_id = format!("snapshot-{snapshot_id}");
    let mut cursor: Option<String> = None;
    let mut seen_cursors = std::collections::BTreeSet::new();
    let mut matched: Option<RemoteObject> = None;
    for _ in 0..MAX_DISCOVERY_PAGES {
        cancel.check()?;
        let page = connected
            .provider
            .list_objects(
                &connected.handle,
                Collection::Snapshots,
                cursor.as_deref(),
                SNAPSHOT_DISCOVERY_PAGE_LIMIT,
                cancel,
            )
            .await?;
        if page.objects.len() > SNAPSHOT_DISCOVERY_PAGE_LIMIT as usize {
            return Err(corrupt("snapshot discovery page exceeds limit"));
        }
        for receipt in page.objects {
            cancel.check()?;
            let (object, view) = open_listed_snapshot(connected, receipt, cancel).await?;
            if view.snapshot_id == snapshot_id {
                if object.object_id != expected_object_id {
                    return Err(corrupt("snapshot selection identity differs"));
                }
                if let Some(previous) = &matched {
                    if previous.ciphertext_sha256 != object.ciphertext_sha256
                        || previous.plaintext_sha256 != object.plaintext_sha256
                        || previous.receipt.byte_length != object.receipt.byte_length
                    {
                        return Err(corrupt("ambiguous snapshot discovery"));
                    }
                } else {
                    matched = Some(object);
                }
            }
        }
        let Some(next) = page.next_cursor else {
            return matched.ok_or_else(|| ProviderError::new(ErrorKind::NotFound));
        };
        if next.is_empty()
            || next.len() > 4096
            || next.chars().any(char::is_control)
            || !seen_cursors.insert(next.clone())
        {
            return Err(corrupt("invalid snapshot discovery cursor"));
        }
        cursor = Some(next);
    }
    Err(corrupt("snapshot discovery page limit"))
}

fn same_snapshot_identity(left: &RemoteObject, right: &RemoteObject) -> bool {
    left.repository_id == right.repository_id
        && left.object_id == right.object_id
        && left.role == right.role
        && left.receipt.byte_length == right.receipt.byte_length
        && left.receipt.complete == right.receipt.complete
        && left.ciphertext_sha256 == right.ciphertext_sha256
        && left.plaintext_length == right.plaintext_length
        && left.plaintext_sha256 == right.plaintext_sha256
}

/// Reads an authenticated locator first. Only an explicit NotFound means the
/// provider may be scanned to recover an object's current opaque locator.
pub(crate) async fn find_snapshot_with_locator(
    connected: &super::connection_commands::ConnectedRepository,
    snapshot_id: &str,
    known: Option<&RemoteObject>,
    cancel: &Cancellation,
) -> Result<RemoteObject> {
    find_snapshot_with_locator_invalidation(connected, snapshot_id, known, || Ok(()), cancel).await
}

pub(crate) async fn find_snapshot_with_locator_invalidation<F>(
    connected: &super::connection_commands::ConnectedRepository,
    snapshot_id: &str,
    known: Option<&RemoteObject>,
    invalidate_known: F,
    cancel: &Cancellation,
) -> Result<RemoteObject>
where
    F: FnOnce() -> Result<()>,
{
    if snapshot_id.is_empty() || snapshot_id.len() > 1024 || snapshot_id.contains('\0') {
        return Err(corrupt("invalid snapshot selection"));
    }
    if let Some(snapshot) = known {
        match open_known_snapshot_document(connected, snapshot, cancel).await {
            Ok((object, view)) => {
                if view.snapshot_id != snapshot_id {
                    return Err(corrupt("snapshot selection identity differs"));
                }
                return Ok(object);
            }
            Err(error) if error.kind == ErrorKind::NotFound => invalidate_known()?,
            Err(error) => return Err(error),
        }
    }
    let recovered = scan_snapshot(connected, snapshot_id, cancel).await?;
    if known.is_some_and(|expected| !same_snapshot_identity(expected, &recovered)) {
        return Err(corrupt("relocated snapshot identity differs"));
    }
    Ok(recovered)
}

/// ID-only recovery for callers that do not yet hold an authenticated object
/// reference. Callers with a stored locator use `find_snapshot_with_locator`.
pub(crate) async fn find_snapshot(
    connected: &super::connection_commands::ConnectedRepository,
    snapshot_id: &str,
    cancel: &Cancellation,
) -> Result<RemoteObject> {
    find_snapshot_with_locator(connected, snapshot_id, None, cancel).await
}

/// What a listing or a restore needs from either published document. The
/// authenticated envelope role, not the object name, decides which one was
/// opened, so a bundle can never be read back as a state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SnapshotView {
    pub snapshot_id: String,
    /// The state this one replaced, when the document names one. A bundle
    /// wrapped around a capture has no lineage of its own.
    pub parent_snapshot_id: Option<String>,
    pub library_id: String,
    pub created_at_ms: u64,
    /// The local library revision for a bundle, or the remote commit order for
    /// a published state. These are different counters and are never mixed.
    pub revision: String,
    pub library: wire::LibrarySnapshotRef,
    pub original_units: Option<wire::StoredObject>,
    pub sections: std::collections::BTreeMap<String, wire::SectionSnapshotRef>,
    pub is_state: bool,
    /// The device whose own values these sections are, when there is one. A
    /// published state and a bundle wrapped around one hold merged material
    /// instead, so neither names a device.
    pub captured_by_device: Option<String>,
}

impl SnapshotView {
    pub(crate) fn read(plaintext: &[u8], role: wire::ObjectRole, repository_id: &str) -> Result<Self> {
        let view = match role {
            wire::ObjectRole::SyncState => {
                let document = wire::SyncStateDocument::decode(plaintext, wire::MAX_METADATA_BYTES);
                #[cfg(test)] observe_state_result(&document,2);
                let document=document.map_err(corrupt)?;
                Self {
                    snapshot_id: document.state_id,
                    parent_snapshot_id: document.parent_state_id,
                    library_id: document.library_id,
                    created_at_ms: document.created_at_ms,
                    revision: document.generation.as_str().to_owned(),
                    library: document.library,
                    sections: document.sections,
                    is_state: true,
                    captured_by_device: None,
                    original_units: None,
                }
            }
            wire::ObjectRole::BackupBundle => {
                let document=wire_control::BackupBundleDocument::decode(plaintext, MAX_POINT_PLAINTEXT);
                #[cfg(test)] observe_bundle_result(&document,1);
                let document=document.map_err(corrupt)?;
                Self {
                    snapshot_id: document.bundle_id,
                    parent_snapshot_id: None,
                    library_id: repository_id.to_owned(),
                    created_at_ms: document.captured_at_ms,
                    revision: document
                        .local_library_revision
                        .as_ref()
                        .map(|value| value.as_str().to_owned())
                        .unwrap_or_else(|| "0".into()),
                    captured_by_device: match &document.source {
                        wire_control::BundleSource::Device { writer_id } => {
                            Some(writer_id.clone())
                        }
                        wire_control::BundleSource::SyncState { .. } => None,
                    },
                    library: document.library,
                    original_units: document.original_units,
                    sections: document.sections,
                    is_state: false,
                }
            }
            _ => return Err(corrupt("unsupported snapshot document role")),
        };
        if view.library.record_catalog.header.repository_id != repository_id
            || view.library.asset_catalog.header.repository_id != repository_id
        {
            return Err(corrupt("snapshot document binding differs"));
        }
        Ok(view)
    }
}

async fn open_known_snapshot_document(
    connected: &super::connection_commands::ConnectedRepository,
    snapshot: &RemoteObject,
    cancel: &Cancellation,
) -> Result<(RemoteObject, SnapshotView)> {
    if !matches!(
        snapshot.role,
        ObjectRole::SyncState | ObjectRole::BackupBundle
    ) || snapshot.repository_id != connected.stored.descriptor.repository_id
        || snapshot.receipt.byte_length == 0
        || snapshot.receipt.byte_length > MAX_SNAPSHOT_CIPHERTEXT
    {
        return Err(corrupt("invalid selected snapshot"));
    }
    snapshot.stored(&connected.handle)?;
    let (read, bytes) = download_control(
        connected.provider.as_ref(),
        &connected.handle,
        &snapshot.receipt.locator,
        None,
        snapshot.receipt.byte_length,
        cancel,
    )
    .await?;
    let ReadReceipt::Body(receipt) = read else {
        return Err(corrupt("selected snapshot was not downloaded"));
    };
    if receipt.locator != snapshot.receipt.locator
        || receipt.byte_length != snapshot.receipt.byte_length
        || !receipt.complete
    {
        return Err(corrupt("selected snapshot receipt differs"));
    }
    let bytes = bytes.ok_or_else(|| corrupt("selected snapshot was not downloaded"))?;
    let ciphertext_sha256 = hex::encode(hash(&bytes));
    if bytes.len() as u64 != snapshot.receipt.byte_length
        || ciphertext_sha256 != snapshot.ciphertext_sha256
    {
        return Err(corrupt("snapshot ciphertext differs"));
    }
    let key = derive_key(
        &connected.root_key,
        &connected.stored.descriptor.repository_id,
        "metadata",
    )
    .map_err(corrupt)?;
    let mut plaintext = Vec::new();
    let header = wire::open_envelope(
        &mut Cursor::new(&bytes),
        &mut plaintext,
        &key,
        wire::MAX_METADATA_BYTES as u64,
    )
    .map_err(corrupt)?;
    let plaintext_sha256 = hex::encode(hash(&plaintext));
    if header != snapshot.stored(&connected.handle)?.header
        || plaintext.len() as u64 != snapshot.plaintext_length
        || plaintext_sha256 != snapshot.plaintext_sha256
    {
        return Err(corrupt("snapshot plaintext differs"));
    }
    let view = SnapshotView::read(
        &plaintext,
        header.role,
        &connected.stored.descriptor.repository_id,
    )?;
    if snapshot.object_id != format!("snapshot-{}", view.snapshot_id) {
        return Err(corrupt("snapshot document binding differs"));
    }
    Ok((
        RemoteObject {
            repository_id: header.repository_id,
            object_id: header.object_id,
            role: native_role(header.role)?,
            receipt,
            ciphertext_sha256,
            plaintext_length: header.plaintext_length,
            plaintext_sha256,
        },
        view,
    ))
}

/// Reads a selected snapshot root through its authenticated direct locator.
/// This verifies the remote object's ciphertext, RNX1 header, plaintext hash,
/// repository and scope. Referenced packs remain unverified until restore or a
/// dedicated full-history verification downloads them.
pub(crate) async fn read_snapshot_document(
    connected: &super::connection_commands::ConnectedRepository,
    snapshot: &RemoteObject,
    cancel: &Cancellation,
) -> Result<SnapshotView> {
    open_known_snapshot_document(connected, snapshot, cancel)
        .await
        .map(|(_, view)| view)
}

pub(crate) async fn list_connected_backup_points_page(
    connected: &super::connection_commands::ConnectedRepository,
    cursor: Option<&str>,
    limit: u16,
    cancel: &Cancellation,
) -> Result<BackupPointPage> {
    list_backup_points_page(
        &connected.stored.descriptor,
        &connected.root_key,
        connected.provider.as_ref(),
        &connected.handle,
        cursor,
        limit,
        cancel,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::external_storage::{fake, journal::JobIdentity};
    use crate::persistent_store::sync_selection::CaptureIdentity;
    use std::{collections::BTreeMap, sync::Arc};

    fn descriptor(strategy: Strategy) -> Descriptor {
        Descriptor::new("descriptor-repository".into(), Some(strategy),
        )
        .unwrap()
    }
    fn snapshot(repository: &RepositoryHandle, id: &str) -> RemoteObject {
        let descriptor_id = "descriptor-repository".to_owned();
        let header = wire::PublicObjectHeader::new(
            descriptor_id.clone(),
            format!("snapshot-{id}"),
            wire::ObjectRole::SyncState,
            4,
        )
        .unwrap();
        RemoteObject {
            repository_id: descriptor_id,
            object_id: header.object_id.clone(),
            role: ObjectRole::SyncState,
            receipt: ObjectReceipt {
                locator: RemoteLocator {
                    connection_identity: repository.connection_identity.clone(),
                    collection: None,
                    object: format!("opaque-{id}"),
                },
                byte_length: wire::envelope_length(&header).unwrap(),
                version: None,
                checksum: None,
                complete: true,
            },
            ciphertext_sha256: "11".repeat(32),
            plaintext_length: 4,
            plaintext_sha256: "22".repeat(32),
        }
    }

    fn bundle(repository: &RepositoryHandle, id: &str) -> RemoteObject {
        let descriptor_id = "descriptor-repository".to_owned();
        let header = wire::PublicObjectHeader::new(
            descriptor_id.clone(),
            format!("snapshot-{id}"),
            wire::ObjectRole::BackupBundle,
            4,
        )
        .unwrap();
        RemoteObject {
            repository_id: descriptor_id,
            object_id: header.object_id.clone(),
            role: ObjectRole::BackupBundle,
            receipt: ObjectReceipt {
                locator: RemoteLocator {
                    connection_identity: repository.connection_identity.clone(),
                    collection: None,
                    object: format!("opaque-{id}"),
                },
                byte_length: wire::envelope_length(&header).unwrap(),
                version: None,
                checksum: None,
                complete: true,
            },
            ciphertext_sha256: "11".repeat(32),
            plaintext_length: 4,
            plaintext_sha256: "22".repeat(32),
        }
    }
    fn head(strategy: Strategy, repository: &RepositoryHandle, commit: &str) -> HeadDocument {
        HeadDocument::new(
            &descriptor(strategy),
            "library".into(),
            commit.into(),
            None,
            "33".repeat(32),
            snapshot(repository, "s1"),
        )
        .unwrap()
    }
    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    #[test]
    fn control_sink_receives_a_resumed_body_in_memory_and_verifies_it_once() {
        use crate::external_storage::contract::TransferSink;
        use tokio::io::AsyncWriteExt;
        runtime().block_on(async {
            let cancel = Cancellation::default();
            let body = b"synthetic control body";
            let digest = hex::encode(hash(body));
            let mut sink = ControlSink::new(body.len() as u64);
            sink.open(0, 9, &cancel).await.unwrap().write_all(b"synthetiX").await.unwrap();
            sink.open(8, body.len() as u64 - 8, &cancel).await.unwrap().write_all(&body[8..]).await.unwrap();
            assert_eq!(sink.finish(body.len() as u64, &"0".repeat(64)).await.unwrap_err().kind, ErrorKind::Corrupt);
            sink.finish(body.len() as u64, &digest).await.unwrap();
            assert_eq!(sink.finish(body.len() as u64, &digest).await.unwrap_err().kind, ErrorKind::Corrupt);
            assert_eq!(sink.open(0, 1, &cancel).await.err().map(|error| error.kind), Some(ErrorKind::Corrupt));
            assert_eq!(sink.take().unwrap(), body);
        });
    }

    #[test]
    fn control_sink_refuses_overruns_gaps_and_oversized_bodies() {
        use crate::external_storage::contract::TransferSink;
        use tokio::io::AsyncWriteExt;
        runtime().block_on(async {
            let cancel = Cancellation::default();
            let mut sink = ControlSink::new(8);
            for (offset, length) in [(0, 9), (4, 4), (u64::MAX, 1)] {
                assert_eq!(sink.open(offset, length, &cancel).await.err().map(|error| error.kind), Some(ErrorKind::Corrupt));
            }
            let mut writer = sink.open(0, 4, &cancel).await.unwrap();
            assert_eq!(writer.write_all(b"12345").await.unwrap_err().kind(), std::io::ErrorKind::InvalidData);
            writer.write_all(b"1234").await.unwrap();
            assert_eq!(writer.write_all(b"5").await.unwrap_err().kind(), std::io::ErrorKind::InvalidData);
            assert_eq!(sink.finish(9, &hex::encode(hash(b"123456789"))).await.unwrap_err().kind, ErrorKind::Corrupt);
            assert_eq!(sink.take().unwrap(), b"1234");
        });
    }

    fn connected(
        provider: Arc<fake::FakeProvider>,
    ) -> super::super::connection_commands::ConnectedRepository {
        let test = fake::loopback_dependencies(fake::MemoryVault::default(), 1_000);
        super::super::connection_commands::ConnectedRepository {
            stored: super::super::connection_store::StoredConnection {
                id: "connection".into(),
                config: super::super::contract::ConnectionConfig {
                    provider: "synthetic".into(),
                    profile: None,
                    endpoint: "https://synthetic.invalid".into(),
                    account_id: "account".into(),
                    location: BTreeMap::new(),
                    oauth_profile: None,
                },
                descriptor: descriptor(Strategy::Cas),
                descriptor_locator: RemoteLocator {
                    connection_identity: fake::repository().connection_identity,
                    collection: None,
                    object: "descriptor".into(),
                },
                provider_repository_id: fake::repository().repository_id,
                credential_ref: "credential".into(),
                root_key_ref: "key".into(),
                recovery_key_ref: "recovery-key".into(),
                retention_policy: None, transfer_concurrency: None,
                capabilities: fake::capabilities(true),
                created_at_ms: 1_000,
                verified_at_ms: 1,
                last_sync_at_ms: None,
                last_backup_at_ms: None,
            },
            provider,
            handle: fake::repository(),
            dependencies: test.dependencies,
            root_key: zeroize::Zeroizing::new([7; 32]),
        }
    }

    fn catalog(repository: &RepositoryHandle, id: &str) -> wire::StoredObject {
        let header = wire::PublicObjectHeader::new(
            "descriptor-repository".into(),
            id.into(),
            wire::ObjectRole::Catalog,
            4,
        )
        .unwrap();
        RemoteObject {
            repository_id: header.repository_id.clone(),
            object_id: header.object_id.clone(),
            role: ObjectRole::Catalog,
            receipt: ObjectReceipt {
                locator: RemoteLocator {
                    connection_identity: repository.connection_identity.clone(),
                    collection: None,
                    object: format!("opaque-{id}"),
                },
                byte_length: wire::envelope_length(&header).unwrap(),
                version: None,
                checksum: None,
                complete: true,
            },
            ciphertext_sha256: "11".repeat(32),
            plaintext_length: 4,
            plaintext_sha256: "22".repeat(32),
        }
        .stored(repository)
        .unwrap()
    }

    fn snapshot_fixture(
        connected: &super::super::connection_commands::ConnectedRepository,
        snapshot_id: &str,
        locator: &str,
        created_at_ms: u64,
    ) -> (RemoteObject, Vec<u8>) {
        let plaintext = wire_control::BackupBundleDocument::new(
            connected.stored.descriptor.repository_id.clone(),
            snapshot_id.into(),
            wire_control::BundleSource::Device {
                writer_id: "writer".into(),
            },
            created_at_ms,
            Some(risunest_sync_wire::head::Sequence::from(1u64)),
            Some(risunest_sync_wire::head::Sequence::from(1u64)),
            None,
            wire::LibrarySnapshotRef {
                record_catalog: catalog(&connected.handle, "records"),
                asset_catalog: catalog(&connected.handle, "assets"),
                content_fingerprint: [3; 32],
            },
            BTreeMap::new(),
            Some(catalog(&connected.handle, "original-units")),
        )
        .unwrap()
        .encode(MAX_POINT_PLAINTEXT)
        .unwrap();
        let object_id = format!("snapshot-{snapshot_id}");
        let bytes = seal(
            &connected.stored.descriptor,
            &connected.root_key,
            &object_id,
            wire::ObjectRole::BackupBundle,
            &plaintext,
        )
        .unwrap();
        (
            RemoteObject {
                repository_id: connected.stored.descriptor.repository_id.clone(),
                object_id,
                role: ObjectRole::BackupBundle,
                receipt: ObjectReceipt {
                    locator: RemoteLocator {
                        connection_identity: connected.handle.connection_identity.clone(),
                        collection: None,
                        object: locator.into(),
                    },
                    byte_length: bytes.len() as u64,
                    version: None,
                    checksum: None,
                    complete: true,
                },
                ciphertext_sha256: hex::encode(hash(&bytes)),
                plaintext_length: plaintext.len() as u64,
                plaintext_sha256: hex::encode(hash(&plaintext)),
            },
            bytes,
        )
    }

    #[test]
    fn head_uses_descriptor_identity_and_authenticates_the_exact_ciphertext() {
        let repository = fake::repository();
        let descriptor = descriptor(Strategy::Cas);
        let prepared = prepare_head(
            &descriptor,
            &[7; 32],
            &repository,
            head(Strategy::Cas, &repository, "c1"),
        )
        .unwrap();
        assert_eq!(
            prepared.authenticated_body_hash,
            hex::encode(hash(prepared.bytes.as_bytes()))
        );
        let (plaintext, header, _, ciphertext_hash) = open(
            &descriptor,
            &[7; 32],
            Some(HEAD_OBJECT_ID),
            wire::ObjectRole::Head,
            prepared.bytes.as_bytes(),
            MAX_CONTROL_PLAINTEXT,
        )
        .unwrap();
        let opened = wire_control::HeadDocument::decode(&plaintext, MAX_CONTROL_PLAINTEXT).unwrap();
        assert_eq!(opened.commit_id, "c1");
        assert_eq!(header.repository_id, descriptor.repository_id);
        assert_eq!(ciphertext_hash, prepared.authenticated_body_hash);

        let mut tampered = prepared.bytes.as_bytes().to_vec();
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        assert!(open(
            &descriptor,
            &[7; 32],
            Some(HEAD_OBJECT_ID),
            wire::ObjectRole::Head,
            &tampered,
            MAX_CONTROL_PLAINTEXT
        )
        .is_err());
    }

    #[test]
    fn locator_hit_reads_only_the_target_among_one_thousand_snapshots() {
        runtime().block_on(async {
            let provider = Arc::new(fake::FakeProvider::new(false));
            let connected = connected(provider.clone());
            for index in 0..999 {
                provider.seed(
                    &format!("noise-{index:04}"),
                    ObjectRole::BackupBundle,
                    vec![index as u8],
                );
            }
            let (known, bytes) = snapshot_fixture(&connected, "target", "opaque-target", 1);
            provider.seed("opaque-target", ObjectRole::BackupBundle, bytes);

            let found = find_snapshot_with_locator(
                &connected,
                "target",
                Some(&known),
                &Cancellation::default(),
            )
            .await
            .unwrap();

            assert_eq!(found.object_id, "snapshot-target");
            assert_eq!(provider.read_attempts("opaque-target"), 1);
            assert_eq!(provider.listing_count(), 0);
        });
    }

    #[test]
    fn locator_hash_mismatch_fails_without_fallback_listing() {
        runtime().block_on(async {
            let provider = Arc::new(fake::FakeProvider::new(false));
            let connected = connected(provider.clone());
            let (mut known, bytes) = snapshot_fixture(&connected, "target", "opaque-target", 1);
            provider.seed("opaque-target", ObjectRole::BackupBundle, bytes);
            known.ciphertext_sha256 = "00".repeat(32);

            let error = find_snapshot_with_locator(
                &connected,
                "target",
                Some(&known),
                &Cancellation::default(),
            )
            .await
            .unwrap_err();

            assert_eq!(error.kind, ErrorKind::Corrupt);
            assert_eq!(provider.read_attempts("opaque-target"), 1);
            assert_eq!(provider.listing_count(), 0);
        });
    }

    #[test]
    fn locator_authentication_and_permission_errors_do_not_fallback() {
        runtime().block_on(async {
            for kind in [ErrorKind::Unauthorized, ErrorKind::ReauthRequired] {
                let provider = Arc::new(fake::FakeProvider::new(false));
                let connected = connected(provider.clone());
                let (known, bytes) = snapshot_fixture(&connected, "target", "opaque-target", 1);
                provider.seed("opaque-target", ObjectRole::BackupBundle, bytes);
                provider.fail_read("opaque-target", kind);
                let invalidated = std::sync::atomic::AtomicBool::new(false);

                let error = find_snapshot_with_locator_invalidation(
                    &connected,
                    "target",
                    Some(&known),
                    || {
                        invalidated.store(true, std::sync::atomic::Ordering::Release);
                        Ok(())
                    },
                    &Cancellation::default(),
                )
                .await
                .unwrap_err();

                assert_eq!(error.kind, kind);
                assert!(!invalidated.load(std::sync::atomic::Ordering::Acquire));
                assert_eq!(provider.listing_count(), 0);
            }
        });
    }

    #[test]
    fn confirmed_locator_not_found_falls_back_to_authenticated_scan() {
        runtime().block_on(async {
            let provider = Arc::new(fake::FakeProvider::new(false));
            let connected = connected(provider.clone());
            let (mut known, bytes) = snapshot_fixture(&connected, "target", "current-target", 1);
            provider.seed("current-target", ObjectRole::BackupBundle, bytes);
            known.receipt.locator.object = "stale-target".into();
            let invalidated = std::sync::atomic::AtomicBool::new(false);

            let found = find_snapshot_with_locator_invalidation(
                &connected,
                "target",
                Some(&known),
                || {
                    invalidated.store(true, std::sync::atomic::Ordering::Release);
                    Ok(())
                },
                &Cancellation::default(),
            )
            .await
            .unwrap();

            assert!(invalidated.load(std::sync::atomic::Ordering::Acquire));
            assert_eq!(found.receipt.locator.object, "current-target");
            assert_eq!(provider.read_attempts("stale-target"), 1);
            assert_eq!(provider.listing_count(), 1);
        });
    }

    #[test]
    fn scan_rejects_ambiguous_authenticated_snapshot_matches() {
        runtime().block_on(async {
            let provider = Arc::new(fake::FakeProvider::new(false));
            let connected = connected(provider.clone());
            let (_, first) = snapshot_fixture(&connected, "duplicate", "first", 1);
            let (_, second) = snapshot_fixture(&connected, "duplicate", "second", 2);
            provider.seed("first", ObjectRole::BackupBundle, first);
            provider.seed("second", ObjectRole::BackupBundle, second);

            let error = find_snapshot(&connected, "duplicate", &Cancellation::default())
                .await
                .unwrap_err();

            assert_eq!(error.kind, ErrorKind::Corrupt);
            assert_eq!(provider.listing_count(), 1);
        });
    }

    #[test]
    fn relocated_snapshot_with_the_same_id_but_different_bytes_is_rejected() {
        runtime().block_on(async {
            let provider = Arc::new(fake::FakeProvider::new(false));
            let connected = connected(provider.clone());
            let (mut known, _) = snapshot_fixture(&connected, "target", "stale-target", 1);
            let (_, changed) = snapshot_fixture(&connected, "target", "current-target", 2);
            provider.seed("current-target", ObjectRole::BackupBundle, changed);
            known.receipt.locator.object = "stale-target".into();

            let error = find_snapshot_with_locator(
                &connected,
                "target",
                Some(&known),
                &Cancellation::default(),
            )
            .await
            .unwrap_err();

            assert_eq!(error.kind, ErrorKind::Corrupt);
            assert_eq!(provider.read_attempts("stale-target"), 1);
            assert_eq!(provider.listing_count(), 1);
        });
    }

    #[test]
    fn scan_rejects_a_repeated_discovery_cursor() {
        runtime().block_on(async {
            let provider = Arc::new(fake::FakeProvider::new(false));
            let connected = connected(provider.clone());
            for _ in 0..2 {
                provider.script_page(
                    Collection::Snapshots,
                    Ok(super::super::contract::ObjectPage {
                        objects: Vec::new(),
                        next_cursor: Some("repeat".into()),
                    }),
                );
            }

            let error = find_snapshot(&connected, "missing", &Cancellation::default())
                .await
                .unwrap_err();

            assert_eq!(error.kind, ErrorKind::Corrupt);
            assert_eq!(provider.listing_count(), 2);
        });
    }

    #[test]
    fn a_backup_point_keeps_one_bundle_and_uploads_once_at_its_fixed_id() {
        runtime().block_on(async {
            let provider = fake::FakeProvider::new(false);
            let repository = fake::repository();
            let descriptor = descriptor(Strategy::Sequential);
            assert!(BackupPointDocument::single(
                &descriptor,
                "manual-1".into(),
                BackupPointKind::Manual,
                1,
                snapshot(&repository, "s1"),
            )
            .is_err());
            let document = BackupPointDocument::single(
                &descriptor,
                "manual-1".into(),
                BackupPointKind::Manual,
                1,
                bundle(&repository, "s2"),
            )
            .unwrap();
            assert_eq!(document.bundles().len(), 1);
            let root = tempfile::tempdir().unwrap();
            let identity = JobIdentity {
                job_id: "job".into(),
                connection_id: "connection".into(),
                repository_id: repository.repository_id.clone(),
                capture_id: "capture".into(),
                capture: CaptureIdentity {
                    store_id: "store".into(),
                    library_epoch: "epoch".into(),
                    generation: "generation".into(),
                    selection_epoch: "selection".into(),
                    revision: 1,
                },
            };
            let mut journal = TransferJournal::open(root.path(), identity).unwrap();
            let uploaded = upload_backup_point(
                &descriptor,
                &[6; 32],
                document.clone(),
                &mut journal,
                &provider,
                &repository,
                &Cancellation::default(),
            )
            .await
            .unwrap();
            assert_eq!(uploaded.repository_id, descriptor.repository_id);
            assert_eq!(uploaded.role, ObjectRole::BackupPoint);
            assert_eq!(uploaded.object_id, "backup-point-manual-1");
            let repeated = upload_backup_point(
                &descriptor,
                &[6; 32],
                document,
                &mut journal,
                &provider,
                &repository,
                &Cancellation::default(),
            )
            .await
            .unwrap();
            assert_eq!(repeated.object_id, uploaded.object_id);
            assert_eq!(provider.state.lock().unwrap().objects.len(), 2);
            assert_eq!(provider.state.lock().unwrap().objects.keys()
                .filter(|id| id.starts_with("inventory-page-")).count(), 1);
            assert_eq!(provider.upload_attempts("backup-point-manual-1"), 1);
        });
    }

    #[test]
    fn ordinary_point_delete_requires_the_exact_authenticated_kind_and_observation() {
        runtime().block_on(async {
            let provider = Arc::new(fake::FakeProvider::new(false));
            let connected = connected(provider);
            let root = tempfile::tempdir().unwrap();
            let identity = JobIdentity {
                job_id: "ordinary-operation".into(),
                connection_id: "connection".into(),
                repository_id: connected.handle.repository_id.clone(),
                capture_id: "capture".into(),
                capture: CaptureIdentity {
                    store_id: "store".into(),
                    library_epoch: "epoch".into(),
                    generation: "generation".into(),
                    selection_epoch: "selection".into(),
                    revision: 1,
                },
            };
            let mut journal = TransferJournal::open(root.path(), identity).unwrap();
            let point = BackupPointDocument::single(
                &connected.stored.descriptor,
                "ordinary".into(),
                BackupPointKind::Manual,
                1,
                bundle(&connected.handle, "bundle"),
            ).unwrap();
            let uploaded = upload_backup_point(
                &connected.stored.descriptor,
                &connected.root_key,
                point,
                &mut journal,
                connected.provider.as_ref(),
                &connected.handle,
                &Cancellation::default(),
            ).await.unwrap();
            let stored = uploaded.stored(&connected.handle).unwrap();

            let error = delete_authenticated_backup_point(
                &connected,
                "ordinary",
                BackupPointKind::Automatic,
                &stored,
                &Cancellation::default(),
            ).await.unwrap_err();
            assert_eq!(error.kind, ErrorKind::PreconditionFailed);
            let mut changed = stored.clone();
            changed.plaintext_sha256[0] ^= 1;
            let error = delete_authenticated_backup_point(
                &connected,
                "ordinary",
                BackupPointKind::Manual,
                &changed,
                &Cancellation::default(),
            ).await.unwrap_err();
            assert_eq!(error.kind, ErrorKind::PreconditionFailed);
            assert_eq!(delete_authenticated_backup_point(
                &connected,
                "ordinary",
                BackupPointKind::Manual,
                &stored,
                &Cancellation::default(),
            ).await.unwrap(), RemoteBackupPointDeleteOutcome::Deleted);
        });
    }

    /// A control object is charged to the transfer spool before its file is
    /// written. While another job holds the spool, a point is refused without
    /// leaving a file; once there is room it is sent with its page, and
    /// neither is left behind.
    #[test]
    fn a_backup_point_waits_for_spool_room_before_it_is_written() {
        runtime().block_on(async {
            let provider = Arc::new(fake::FakeProvider::new(false));
            let connected = connected(provider.clone());
            let root = tempfile::tempdir().unwrap();
            let peer = tempfile::tempdir().unwrap();
            let job = format!("point-{}", uuid::Uuid::new_v4());
            let identity = JobIdentity {
                job_id: job.clone(),
                connection_id: "connection".into(),
                repository_id: connected.handle.repository_id.clone(),
                capture_id: "capture".into(),
                capture: CaptureIdentity {
                    store_id: "store".into(),
                    library_epoch: "epoch".into(),
                    generation: "generation".into(),
                    selection_epoch: "selection".into(),
                    revision: 1,
                },
            };
            let limit = 16 * 1024;
            let held = peer.path().join("held.spool");
            fs::write(&held, vec![0u8; limit as usize - 1024]).unwrap();
            let mut journal = TransferJournal::open(root.path(), identity).unwrap();
            let holders = vec![
                (job.clone(), root.path().to_path_buf()),
                ("peer".to_owned(), peer.path().to_path_buf()),
            ];
            journal.set_spool_budget(
                super::super::journal::SpoolBudget::new(job, move || Ok(holders.clone()))
                    .with_limit(limit),
            );
            let point = || {
                BackupPointDocument::single(
                    &connected.stored.descriptor,
                    "charged".into(),
                    BackupPointKind::Manual,
                    1,
                    bundle(&connected.handle, "bundle"),
                )
                .unwrap()
            };
            let error = upload_backup_point(
                &connected.stored.descriptor,
                &connected.root_key,
                point(),
                &mut journal,
                connected.provider.as_ref(),
                &connected.handle,
                &Cancellation::default(),
            )
            .await
            .unwrap_err();
            assert_eq!(error.kind, ErrorKind::Transient);
            assert_eq!(super::super::journal::held_spool_bytes(root.path()).unwrap(), 0);
            assert_eq!(journal.record("backup-point-charged").unwrap().map(|_| ()), None);
            assert!(provider.uploaded_ids().is_empty());

            fs::write(&held, b"").unwrap();
            upload_backup_point(
                &connected.stored.descriptor,
                &connected.root_key,
                point(),
                &mut journal,
                connected.provider.as_ref(),
                &connected.handle,
                &Cancellation::default(),
            )
            .await
            .unwrap();
            let uploaded = provider.uploaded_ids();
            assert!(uploaded.iter().any(|id| id == "backup-point-charged"));
            assert!(uploaded.iter().any(|id| id.starts_with("inventory-page-")));
            assert_eq!(super::super::journal::held_spool_bytes(root.path()).unwrap(), 0);
        });
    }

    /// A control object whose file was written but never registered, as a
    /// stopped write leaves it, is sealed again at the same path and sent with
    /// the bytes the journal registers, not the ones left behind.
    #[test]
    fn a_control_object_replaces_a_spool_file_it_never_registered() {
        runtime().block_on(async {
            let provider = Arc::new(fake::FakeProvider::new(false));
            let connected = connected(provider.clone());
            let root = tempfile::tempdir().unwrap();
            let mut journal = TransferJournal::open(
                root.path(),
                JobIdentity {
                    job_id: format!("orphan-{}", uuid::Uuid::new_v4()),
                    connection_id: "connection".into(),
                    repository_id: connected.handle.repository_id.clone(),
                    capture_id: "capture".into(),
                    capture: CaptureIdentity {
                        store_id: "store".into(),
                        library_epoch: "epoch".into(),
                        generation: "generation".into(),
                        selection_epoch: "selection".into(),
                        revision: 1,
                    },
                },
            )
            .unwrap();
            let left = vec![7u8; 333];
            fs::write(journal.spool_path("backup-point-orphan"), &left).unwrap();
            let object = upload_backup_point(
                &connected.stored.descriptor,
                &connected.root_key,
                BackupPointDocument::single(
                    &connected.stored.descriptor,
                    "orphan".into(),
                    BackupPointKind::Manual,
                    1,
                    bundle(&connected.handle, "bundle"),
                )
                .unwrap(),
                &mut journal,
                connected.provider.as_ref(),
                &connected.handle,
                &Cancellation::default(),
            )
            .await
            .unwrap();
            let record = journal.record("backup-point-orphan").unwrap().unwrap();
            let stored = provider.contents(&object.receipt.locator.object).unwrap();
            assert_eq!(stored.len() as u64, record.intent.byte_length);
            assert_eq!(hex::encode(hash(&stored)), record.intent.sha256);
            assert_eq!(object.ciphertext_sha256, record.intent.sha256);
            assert_ne!(hex::encode(hash(&left)), record.intent.sha256);
            assert!(record.released);
            assert_eq!(super::super::journal::held_spool_bytes(root.path()).unwrap(), 0);
        });
    }

    /// A wave reserves spool room for the page that registers it, so the page
    /// it seals has to fit that room from one member to a full wave.
    #[test]
    fn an_inventory_page_fits_the_room_its_wave_reserves() {
        let connected = connected(Arc::new(fake::FakeProvider::new(false)));
        let descriptor = Descriptor::new(uuid::Uuid::new_v4().to_string(), None).unwrap();
        let job = uuid::Uuid::new_v4().to_string();
        let key = [7u8; 32];
        let intents = (0..super::super::transfer_job::REGISTRATION_WAVE)
            .map(|index| ObjectIntent {
                repository_id: connected.handle.repository_id.clone(),
                job_id: job.clone(),
                object_id: wire::keyed_object_id(
                    &key,
                    &job,
                    wire::ObjectRole::Catalog,
                    &[index as u8; 32],
                )
                .unwrap(),
                role: ObjectRole::Catalog,
                byte_length: 256 * 1024 * 1024,
                sha256: "11".repeat(32),
            })
            .collect::<Vec<_>>();
        let plaintext_sha256 = "22".repeat(32);
        for members in [1, intents.len()] {
            let registrations = intents[..members]
                .iter()
                .map(|intent| InventoryRegistration {
                    intent,
                    plaintext_length: 256 * 1024 * 1024,
                    plaintext_sha256: &plaintext_sha256,
                })
                .collect::<Vec<_>>();
            let page_id = "ab".repeat(32);
            let document = inventory_page_document(
                &descriptor,
                &connected.handle,
                &job,
                &page_id,
                &registrations,
            )
            .unwrap();
            let sealed = seal(
                &descriptor,
                &connected.root_key,
                &format!("inventory-page-{page_id}"),
                wire::ObjectRole::InventoryPage,
                &document.encode(MAX_POINT_PLAINTEXT).unwrap(),
            )
            .unwrap();
            assert!(
                sealed.len() as u64 <= inventory_page_headroom(members),
                "a page of {members} members seals to {} bytes",
                sealed.len()
            );
        }
    }

    #[test]
    fn inventory_page_is_confirmed_listed_and_authenticated_before_delete() {
        runtime().block_on(async {
            let provider = Arc::new(fake::FakeProvider::new(false));
            let connected = connected(provider.clone());
            let root = tempfile::tempdir().unwrap();
            let identity = JobIdentity {
                job_id: "operation".into(),
                connection_id: "connection".into(),
                repository_id: connected.handle.repository_id.clone(),
                capture_id: "capture".into(),
                capture: CaptureIdentity {
                    store_id: "store".into(),
                    library_epoch: "epoch".into(),
                    generation: "generation".into(),
                    selection_epoch: "selection".into(),
                    revision: 1,
                },
            };
            let mut journal = TransferJournal::open(root.path(), identity).unwrap();
            let intent = ObjectIntent {
                repository_id: connected.handle.repository_id.clone(),
                job_id: "operation".into(),
                object_id: "pack-a".into(),
                role: ObjectRole::Pack,
                byte_length: 12,
                sha256: "11".repeat(32),
            };
            let document = inventory_page_document(
                &connected.stored.descriptor,
                &connected.handle,
                "operation",
                "operation-0",
                &[InventoryRegistration {
                    intent: &intent,
                    plaintext_length: 6,
                    plaintext_sha256: &"22".repeat(32),
                }],
            )
            .unwrap();
            let uploaded = upload_inventory_page(
                &connected.stored.descriptor,
                &connected.root_key,
                document.clone(),
                &mut journal,
                connected.provider.as_ref(),
                &connected.handle,
                &Cancellation::default(),
            )
            .await
            .unwrap();

            let listed = list_inventory_pages_page(
                &connected.stored.descriptor,
                &connected.root_key,
                connected.provider.as_ref(),
                &connected.handle,
                None,
                10,
                &Cancellation::default(),
            )
            .await
            .unwrap();
            assert_eq!(listed.next_cursor, None);
            assert_eq!(listed.pages.len(), 1);
            assert_eq!(listed.pages[0].document, document);
            assert_eq!(listed.pages[0].reference, uploaded);

            let stored = uploaded.stored(&connected.handle).unwrap();
            assert_eq!(
                delete_authenticated_inventory_page(
                    &connected,
                    &stored,
                    &Cancellation::default(),
                )
                .await
                .unwrap(),
                RemoteInventoryPageDeleteOutcome::Deleted
            );
            assert_eq!(provider.delete_attempts("inventory-page-operation-0"), 1);
            assert!(list_inventory_pages_page(
                &connected.stored.descriptor,
                &connected.root_key,
                connected.provider.as_ref(),
                &connected.handle,
                None,
                10,
                &Cancellation::default(),
            )
            .await
            .unwrap()
            .pages
            .is_empty());
        });
    }

}
