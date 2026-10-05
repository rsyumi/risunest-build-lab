use super::contract::{ErrorKind, ProviderError, Result};
use crate::persistent_store::lww::Change;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use risunest_external_storage_format::crypto::{decrypt, derive_key, encrypt};
use risunest_external_storage_format::snapshot::{ObjectRole as WireRole, StoredObject};
use risunest_sync_wire::{
    stamp::{validate_writer_id, DecimalU64},
    unit::compare_version,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Cursor,
};

pub(crate) const SEGMENT_BYTES: usize = 64 * 1024 * 1024;
pub(crate) const SMALL_BODY_BYTES: usize = 4 * 1024 * 1024;

pub(crate) fn corrupt() -> ProviderError {
    ProviderError::new(ErrorKind::Corrupt)
}
pub(crate) fn digest(bytes: &[u8]) -> String {
    #[cfg(test)]
    {
        HASH_BYTES.with(|count| count.set(count.get() + bytes.len() as u64));
        HASH_CALLS.with(|count| count.set(count.get() + 1));
    }
    hex::encode(Sha256::digest(bytes))
}
#[cfg(test)]
thread_local! { static HASH_BYTES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) }; static HASH_CALLS:std::cell::Cell<u64> = const {std::cell::Cell::new(0)}; }
#[cfg(test)]
pub(crate) fn reset_hash_bytes() {
    HASH_BYTES.with(|count| count.set(0));
    HASH_CALLS.with(|count| count.set(0));
}
#[cfg(test)]
pub(crate) fn take_hash_bytes() -> u64 {
    HASH_BYTES.with(|count| count.replace(0))
}
#[cfg(test)]
pub(crate) fn take_hash_calls() -> u64 {
    HASH_CALLS.with(|count| count.replace(0))
}

#[cfg(test)]
thread_local! { static DELEGATED_HASH_WORK: std::cell::RefCell<BTreeMap<&'static str,(u64,u64)>> = const { std::cell::RefCell::new(BTreeMap::new()) }; }
#[cfg(test)]
pub(crate) fn reset_delegated_hash_work() {
    DELEGATED_HASH_WORK.with(|work| work.borrow_mut().clear());
}
#[cfg(test)]
pub(crate) fn take_delegated_hash_work() -> BTreeMap<&'static str, (u64, u64)> {
    DELEGATED_HASH_WORK.with(|work| std::mem::take(&mut *work.borrow_mut()))
}
#[cfg(test)]
pub(crate) fn observe_hash_input(domain: &'static str, bytes: usize) {
    DELEGATED_HASH_WORK.with(|work| {
        let mut work = work.borrow_mut();
        let entry = work.entry(domain).or_default();
        entry.0 += 1;
        entry.1 += bytes as u64;
    });
}
#[cfg(test)]
pub(crate) fn observe_value_validation(
    domain: &'static str,
    value: &risunest_sync_wire::unit::UnitValue,
) {
    if let risunest_sync_wire::unit::UnitValue::Object {
        descriptor_hash,
        descriptor,
    } = value
    {
        if risunest_sync_wire::validate_hash(descriptor_hash).is_ok() {
            if let Ok(bytes) = descriptor.bytes() {
                observe_hash_input(domain, bytes.len());
            }
        }
    }
}
#[cfg(test)]
pub(crate) fn observe_value_identity(
    descriptor_domain: &'static str,
    value_domain: &'static str,
    value: &risunest_sync_wire::unit::UnitValue,
    success: bool,
) {
    observe_value_validation(descriptor_domain, value);
    if success {
        if let Ok(bytes) = risunest_sync_wire::canonical::encode(value) {
            observe_hash_input(value_domain, bytes.len());
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct LargeBody {
    pub object_id: String,
    pub sha256: String,
    pub byte_length: DecimalU64,
    pub plaintext_byte_length: DecimalU64,
    pub locator: Option<super::contract::RemoteLocator>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Segment {
    pub schema: String,
    pub library_id: String,
    pub writer_id: String,
    pub seq: DecimalU64,
    pub changes: Vec<Change>,
    pub message_pages: BTreeMap<String, String>,
    #[serde(with = "super::lww_checkpoint::nested_metadata")]
    pub data_catalogs: Vec<StoredObject>,
    #[serde(with = "super::lww_checkpoint::nested_metadata")]
    pub asset_catalogs: Vec<StoredObject>,
    pub large_bodies: BTreeMap<String, LargeBody>,
}
impl Segment {
    pub(crate) fn new(library: &str, writer: &str, seq: u64) -> Self {
        Self {
            schema: "risunest.segment/v1".into(),
            library_id: library.into(),
            writer_id: writer.into(),
            seq: DecimalU64(seq),
            changes: Vec::new(),
            message_pages: BTreeMap::new(),
            data_catalogs: Vec::new(),
            asset_catalogs: Vec::new(),
            large_bodies: BTreeMap::new(),
        }
    }
    pub(crate) fn validate(&self) -> Result<()> {
        self.validate_inner(true)
    }
    fn validate_inner(&self, prepared: bool) -> Result<()> {
        if self.schema != "risunest.segment/v1" || self.library_id.is_empty() || self.seq.0 == 0 {
            return Err(corrupt());
        }
        validate_writer_id(&self.writer_id).map_err(|_| corrupt())?;
        let mut keys = BTreeSet::new();
        for change in &self.changes {
            change.stamp.validate().map_err(|_| corrupt())?;
            let validation = change.value.validate();
            #[cfg(test)]
            observe_value_validation("segment-validate.descriptor", &change.value);
            validation.map_err(|_| corrupt())?;
            if !keys.insert(change.key.as_str()) {
                return Err(corrupt());
            }
            // A segment publisher can forward units issued by another device.
        }
        for (hash, bytes) in &self.message_pages {
            let body = URL_SAFE_NO_PAD.decode(bytes).map_err(|_| corrupt())?;
            if URL_SAFE_NO_PAD.encode(&body) != *bytes || digest(&body) != *hash {
                return Err(corrupt());
            }
        }
        let mut catalogs = BTreeSet::new();
        let mut repository = None;
        for catalog in self.data_catalogs.iter().chain(&self.asset_catalogs) {
            catalog.validate().map_err(|_| corrupt())?;
            if catalog.header.role != WireRole::Catalog
                || !catalogs.insert(&catalog.header.object_id)
                || repository.is_some_and(|id| id != &catalog.header.repository_id)
            {
                return Err(corrupt());
            }
            repository = Some(&catalog.header.repository_id);
        }
        for (hash, body) in &self.large_bodies {
            risunest_sync_wire::validate_hash(hash).map_err(|_| corrupt())?;
            risunest_sync_wire::validate_hash(&body.sha256).map_err(|_| corrupt())?;
            risunest_sync_wire::validate_id(&body.object_id).map_err(|_| corrupt())?;
            if prepared && (body.byte_length.0 == 0 || body.sha256 == "0".repeat(64)
                || body.plaintext_byte_length.0 <= SMALL_BODY_BYTES as u64 || body.locator.is_none()) {
                return Err(corrupt());
            }
        }
        Ok(())
    }
    pub(crate) fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let value = serde_json::to_value(self).map_err(|_| corrupt())?;
        risunest_sync_wire::canonical::encode(&value).map_err(|_| corrupt())
    }
    pub(super) fn encode_capture(&self) -> Result<Vec<u8>> {
        self.validate_inner(false)?;
        let value = serde_json::to_value(self).map_err(|_| corrupt())?;
        risunest_sync_wire::canonical::encode(&value).map_err(|_| corrupt())
    }
    pub(super) fn decode_capture(bytes: &[u8]) -> Result<Self> {
        let value: Self = risunest_sync_wire::canonical::decode(bytes, bytes.len()).map_err(|_| corrupt())?;
        if value.encode_capture()? != bytes { return Err(corrupt()); }
        Ok(value)
    }
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self> {
        let value: Self =
            risunest_sync_wire::canonical::decode(bytes, bytes.len()).map_err(|_| corrupt())?;
        if value.encode()? != bytes {
            return Err(corrupt());
        }
        Ok(value)
    }
}
fn binding(library: &str, writer: &str, seq: u64, role: &str) -> Result<Vec<u8>> {
    risunest_sync_wire::canonical::encode(&serde_json::json!({
        "schema":"risunest.segment-envelope/v1", "libraryId":library,
        "writerId":writer, "seq":seq.to_string(), "role":role
    }))
    .map_err(|_| corrupt())
}
pub(crate) fn sealed_length(plaintext_length: usize) -> Result<u64> {
    let length = risunest_external_storage_format::crypto::ciphertext_length(plaintext_length as u64)
        .map_err(|_| corrupt())?;
    if length > SEGMENT_BYTES as u64 { return Err(ProviderError::new(super::contract::ErrorKind::FileTooLarge)); }
    Ok(length)
}
pub(crate) fn seal(segment: &Segment, root_key: &[u8; 32]) -> Result<(Vec<u8>, String)> {
    let plaintext = segment.encode()?;
    let expected_length = sealed_length(plaintext.len())?;
    let hash = digest(&plaintext);
    let key = derive_key(root_key, &segment.library_id, "metadata").map_err(|_| corrupt())?;
    let mut sealed = Vec::new();
    encrypt(
        &mut Cursor::new(&plaintext),
        &mut sealed,
        &key,
        &binding(
            &segment.library_id,
            &segment.writer_id,
            segment.seq.0,
            "segment",
        )?,
        plaintext.len() as u64,
    )
    .map_err(|_| corrupt())?;
    if sealed.len() as u64 != expected_length { return Err(corrupt()); }
    Ok((sealed, hash))
}
pub(crate) fn open(
    bytes: &[u8],
    library: &str,
    writer: &str,
    seq: u64,
    root_key: &[u8; 32],
) -> Result<Segment> {
    if bytes.len() > SEGMENT_BYTES { return Err(corrupt()); }
    let key = derive_key(root_key, library, "metadata").map_err(|_| corrupt())?;
    let mut plain = Vec::new();
    decrypt(
        &mut Cursor::new(bytes),
        &mut plain,
        &key,
        &binding(library, writer, seq, "segment")?,
        SEGMENT_BYTES as u64,
    )
    .map_err(|_| corrupt())?;
    let segment = Segment::decode(&plain)?;
    if segment.library_id != library || segment.writer_id != writer || segment.seq.0 != seq {
        return Err(corrupt());
    }
    Ok(segment)
}
pub(crate) fn seal_body_stream(
    input: &mut impl std::io::Read,
    output: &mut impl std::io::Write,
    library: &str,
    object: &str,
    root_key: &[u8; 32],
    length: u64,
) -> Result<()> {
    let key = derive_key(root_key, library, "metadata").map_err(|_| corrupt())?;
    encrypt(input, output, &key, &binding(library, object, 0, "body")?, length).map_err(|_| corrupt())
}
pub(crate) fn open_body_stream(input:&mut impl std::io::Read,output:&mut impl std::io::Write,library:&str,object:&str,root_key:&[u8;32],length:u64)->Result<()> {
    let key=derive_key(root_key,library,"metadata").map_err(|_|corrupt())?;
    if decrypt(input,output,&key,&binding(library,object,0,"body")?,length).map_err(|_|corrupt())? != length {return Err(corrupt());}
    Ok(())
}pub(crate) fn open_body(
    bytes: &[u8],
    library: &str,
    object: &str,
    root_key: &[u8; 32],
) -> Result<Vec<u8>> {
    let key = derive_key(root_key, library, "metadata").map_err(|_| corrupt())?;
    let mut plain = Vec::new();
    decrypt(
        &mut Cursor::new(bytes),
        &mut plain,
        &key,
        &binding(library, object, 0, "body")?,
        u64::MAX,
    )
    .map_err(|_| corrupt())?;
    Ok(plain)
}
pub(crate) fn merge(changes: &mut BTreeMap<String, Change>, incoming: Change) -> Result<()> {
    if let Some(previous) = changes.get(incoming.key.as_str()) {
        let compared = compare_version(
            &previous.stamp,
            &previous.value,
            &incoming.stamp,
            &incoming.value,
        );
        #[cfg(test)]
        if compared.is_ok()
            || compared
                .as_ref()
                .err()
                .is_some_and(|e| e.0 == "equal-stamp-integrity")
        {
            observe_value_identity(
                "segment-merge.descriptor",
                "segment-merge.identity",
                &previous.value,
                true,
            );
            observe_value_identity(
                "segment-merge.descriptor",
                "segment-merge.identity",
                &incoming.value,
                true,
            );
        }
        let decision = compared.map_err(|_| corrupt())?;
        if decision != risunest_sync_wire::unit::LwwDecision::ApplyRemote {
            return Ok(());
        }
    }
    changes.insert(incoming.key.as_str().into(), incoming);
    Ok(())
}
