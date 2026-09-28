use super::{Result, SyncError};
use crate::{
    asset_repository::PayloadCas, persistent_store::server_sync_projection::ServerPayload,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use risunest_sync_wire::{
    canonical,
    descriptor::{build_reference_tree, visit_reference_tree, RecordDescriptor},
    payload, RecordVersion, WireError, MAX_METADATA_BYTES,
};
use risunest_small_object_store as small_object_store;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    io::{Cursor, Read},
    path::Path,
};

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RecordObject {
    schema: String,
    payload: RecordContent,
}
const INLINE_PAYLOAD_BYTES: usize = 16 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
enum RecordContent {
    Inline {
        #[serde(rename = "bytesBase64url")]
        bytes_base64url: String,
    },
    Tree {
        value: payload::Payload,
    },
}

/// Canonical unpadded base64url. Re-encoding equality rejects a non-canonical
/// encoding of the same bytes, so one body has exactly one wrapper.
fn decode_inline(encoded: &str) -> Result<Vec<u8>> {
    let invalid = || SyncError::new("invalid-inline-payload", 409);
    if encoded.len() > INLINE_PAYLOAD_BYTES.div_ceil(3) * 4 {
        return Err(invalid());
    }
    let bytes = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| invalid())?;
    if bytes.len() > INLINE_PAYLOAD_BYTES || URL_SAFE_NO_PAD.encode(&bytes) != encoded {
        return Err(invalid());
    }
    Ok(bytes)
}

/// Bodies at or below this stay in the cache's object database, where a group
/// of them becomes durable together. The threshold decides where a new body
/// goes; where an existing one lives is decided by which store holds it.
pub(crate) const SMALL_OBJECT_BYTES: usize = 64 * 1024;
/// Section 4's collection rule for one group of small bodies.
const BATCH_ROWS: usize = 256;
const BATCH_BYTES: usize = 8 * 1024 * 1024;

pub(crate) use small_object_store::Body;

#[derive(Default)]
struct Staged {
    /// Bodies a caller has been given an identity for and that are not durable
    /// yet, each tagged with the scope depth that staged it. A reader on this
    /// cache sees them exactly as it sees written ones.
    bodies: std::collections::BTreeMap<String, (usize, Vec<u8>)>,
    bytes: usize,
    /// How many scopes are open. At zero every body is written as soon as its
    /// identity is handed out.
    depth: usize,
}

/// What a projection does with the bodies it builds.
#[derive(Clone, Copy)]
enum Emission {
    Persist,
    Identity,
}

#[cfg(test)]
thread_local! {
    static PUT_CALLS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(crate) fn take_put_calls() -> u64 {
    PUT_CALLS.with(|calls| calls.replace(0))
}

pub(crate) struct Cache {
    pub cas: PayloadCas,
    library: Option<PayloadCas>,
    objects: std::sync::Mutex<rusqlite::Connection>,
    staged: std::sync::Mutex<Staged>,
    root: std::path::PathBuf,
    /// What the checkpoint after the last group left in the log.
    checkpoint: std::sync::Mutex<small_object_store::Checkpoint>,
    wal_high_water: u64,
}

/// Holds one group of small bodies open. Committing writes the group; dropping
/// it without committing discards what is still pending, so no reference can
/// be recorded against a body this cache never made durable. Handles release
/// in the order they were taken, innermost first.
pub(crate) struct Batch<'a> {
    cache: &'a Cache,
    /// The scope depth this handle opened. Only bodies staged at or below it
    /// are this handle's to publish or discard, so a nested scope never
    /// decides for the one around it.
    depth: usize,
}

impl Batch<'_> {
    pub fn commit(self) -> Result<()> {
        let depth = self.depth;
        let cache = self.cache;
        std::mem::forget(self);
        let mut staged = cache.staged()?;
        let result = cache.write_staged(&mut staged, depth);
        staged.depth = depth - 1;
        result
    }
}

impl Drop for Batch<'_> {
    fn drop(&mut self) {
        if let Ok(mut staged) = self.cache.staged() {
            let depth = self.depth;
            staged.bodies.retain(|_, (staged_at, _)| *staged_at < depth);
            staged.bytes = staged.bodies.values().map(|(_, body)| body.len()).sum();
            staged.depth = depth - 1;
        }
    }
}

pub(crate) struct ProjectedRecord {
    pub version: RecordVersion,
    pub local_hash: String,
    pub objects: Vec<String>,
}
impl Cache {
    pub fn open(root: &Path) -> Result<Self> {
        std::fs::create_dir_all(root)?;
        let objects = rusqlite::Connection::open(root.join("objects.sqlite"))?;
        // Incremental, so cleanup can hand freed pages back to the filesystem
        // without rewriting the database. It must precede the first table.
        objects.execute_batch("PRAGMA auto_vacuum=INCREMENTAL;")?;
        objects.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA busy_timeout=5000; PRAGMA journal_size_limit=67108864;",
        )?;
        small_object_store::initialize(&objects)
            .map_err(|_| SyncError::new("cache-store-unavailable", 503))?;
        Ok(Self {
            cas: PayloadCas::new(root)?,
            library: None,
            objects: std::sync::Mutex::new(objects),
            staged: std::sync::Mutex::new(Staged::default()),
            root: root.to_path_buf(),
            checkpoint: std::sync::Mutex::new(small_object_store::Checkpoint::UNKNOWN),
            wal_high_water: small_object_store::WAL_HIGH_WATER_BYTES,
        })
    }
    #[cfg(test)]
    pub fn with_wal_high_water(mut self, bytes: u64) -> Self {
        self.wal_high_water = bytes;
        self
    }
    pub fn with_library(mut self, root: &Path) -> Result<Self> {
        self.library = Some(PayloadCas::new(root)?);
        Ok(self)
    }
    fn staged(&self) -> Result<std::sync::MutexGuard<'_, Staged>> {
        self.staged
            .lock()
            .map_err(|_| SyncError::new("cache-store-unavailable", 503))
    }
    fn objects(&self) -> Result<std::sync::MutexGuard<'_, rusqlite::Connection>> {
        self.objects
            .lock()
            .map_err(|_| SyncError::new("cache-store-unavailable", 503))
    }
    /// Collect small bodies until the scope is committed, so one group of them
    /// becomes durable together instead of one publication each.
    pub fn begin_batch(&self) -> Result<Batch<'_>> {
        let mut staged = self.staged()?;
        staged.depth += 1;
        Ok(Batch {
            cache: self,
            depth: staged.depth,
        })
    }
    /// Write every body staged at or below `depth`. Bodies the scopes around
    /// this one staged stay pending; their own scope decides when they land.
    /// A log readers are holding or a volume without room refuses the group;
    /// groups already written stay written.
    fn write_staged(&self, staged: &mut Staged, depth: usize) -> Result<()> {
        let group = staged
            .bodies
            .iter()
            .filter(|(_, (staged_at, _))| *staged_at >= depth)
            .map(|(hash, (_, body))| (hash.clone(), body.clone()))
            .collect::<Vec<_>>();
        if group.is_empty() {
            return Ok(());
        }
        {
            let mut objects = self.objects()?;
            let mut checkpoint = self
                .checkpoint
                .lock()
                .map_err(|_| SyncError::new("cache-store-unavailable", 503))?;
            let bytes = group.iter().map(|(_, body)| body.len() as u64).sum();
            let available = fs2::available_space(&self.root)?;
            match small_object_store::admit(
                &objects,
                &mut checkpoint,
                self.wal_high_water,
                bytes,
                available,
            )
            .map_err(|_| SyncError::new("cache-store-unavailable", 503))?
            {
                None => (),
                Some(small_object_store::Pressure::Log) => {
                    return Err(SyncError::new("cache-store-busy", 503))
                }
                Some(small_object_store::Pressure::Space) => {
                    return Err(SyncError::new("local-storage-full", 507))
                }
            }
            let tx = objects.transaction()?;
            small_object_store::insert_batch(
                &tx,
                &group
                    .iter()
                    .map(|(hash, bytes)| (hash.as_str(), bytes.as_slice()))
                    .collect::<Vec<_>>(),
            )
            .map_err(|_| SyncError::new("cached-object-corrupt", 409))?;
            tx.commit()?;
            // Between groups, never inside one. A reader on another connection
            // can leave the log where it is; this group is already durable, so
            // a refused checkpoint is not a reason to fail it. What it leaves
            // decides whether the next group is admitted.
            *checkpoint = small_object_store::checkpoint(&objects)
                .unwrap_or(small_object_store::Checkpoint::UNKNOWN);
        }
        for (hash, _) in &group {
            staged.bodies.remove(hash);
        }
        staged.bytes = staged.bodies.values().map(|(_, body)| body.len()).sum();
        Ok(())
    }
    /// The derived cache only, without the library a receive may read through.
    pub fn stat_derived(&self, hash: &str) -> Result<Option<u64>> {
        if let Some((_, bytes)) = self.staged()?.bodies.get(hash) {
            return Ok(Some(bytes.len() as u64));
        }
        let size = small_object_store::size(&*self.objects()?, hash)
            .map_err(|_| SyncError::new("cached-object-corrupt", 409))?;
        if let Some(size) = size {
            return Ok(Some(size));
        }
        Ok(self.cas.stat_object(hash)?)
    }
    pub fn stat_object(&self, hash: &str) -> Result<Option<u64>> {
        if let Some(size) = self.stat_derived(hash)? {
            return Ok(Some(size));
        }
        Ok(match &self.library {
            Some(library) => library.stat_object(hash)?,
            None => None,
        })
    }
    /// The derived cache only, matching `stat_derived`.
    pub fn open_derived(&self, hash: &str) -> Result<Option<Body>> {
        if let Some((_, bytes)) = self.staged()?.bodies.get(hash) {
            return Ok(Some(Body::bytes(bytes.clone())));
        }
        let stored = small_object_store::read(&*self.objects()?, hash, SMALL_OBJECT_BYTES)
            .map_err(|_| SyncError::new("cached-object-corrupt", 409))?;
        if let Some(bytes) = stored {
            return Ok(Some(Body::bytes(bytes)));
        }
        Ok(self.cas.open_object(hash)?.map(Body::File))
    }
    pub fn open_object(&self, hash: &str) -> Result<Option<Body>> {
        if let Some(body) = self.open_derived(hash)? {
            return Ok(Some(body));
        }
        Ok(match &self.library {
            Some(library) => library.open_object(hash)?.map(Body::File),
            None => None,
        })
    }
    pub fn verify(&self, hash: &str, check: impl Fn() -> Result<()>) -> Result<()> {
        use sha2::{Digest, Sha256};
        let mut body = self
            .open_object(hash)?
            .ok_or_else(|| SyncError::new("cached-object-missing", 409))?;
        let mut digest = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        loop {
            check()?;
            let size = body.read(&mut buffer)?;
            if size == 0 {
                break;
            }
            digest.update(&buffer[..size]);
        }
        if hex::encode(digest.finalize()) != hash {
            return Err(SyncError::new("cached-object-corrupt", 409));
        }
        Ok(())
    }
    /// Where a projection's objects go. Persisting is the cache itself; the
    /// identity form names what the projection would hold without writing any
    /// of it, and names it the same, because a stored body is its hash.
    fn emit(&self, emission: Emission, bytes: &[u8]) -> Result<String> {
        match emission {
            Emission::Persist => self.put(bytes),
            Emission::Identity => Ok(risunest_sync_wire::hash(bytes)),
        }
    }

    pub fn put(&self, bytes: &[u8]) -> Result<String> {
        #[cfg(test)]
        PUT_CALLS.with(|calls| calls.set(calls.get() + 1));
        let hash = risunest_sync_wire::hash(bytes);
        if bytes.len() > SMALL_OBJECT_BYTES {
            if self.cas.stat_object(&hash)?.is_some() {
                self.read(&hash, bytes.len())?;
                return Ok(hash);
            }
            return Ok(self
                .cas
                .prepare_reader_expected(&mut Cursor::new(bytes), &hash, bytes.len() as u64)?
                .content_hash);
        }
        if self.stat_derived(&hash)?.is_some() {
            self.read(&hash, bytes.len())?;
            return Ok(hash);
        }
        let mut staged = self.staged()?;
        let depth = staged.depth;
        staged.bytes += bytes.len();
        staged.bodies.insert(hash.clone(), (depth, bytes.to_vec()));
        // A group that reaches section 4's bound is written now. Publishing a
        // body early is safe in a way publishing a reference early is not, so
        // an open scope that later fails only leaves collectable bodies.
        if depth == 0 || staged.bodies.len() >= BATCH_ROWS || staged.bytes >= BATCH_BYTES {
            self.write_staged(&mut staged, 0)?;
        }
        Ok(hash)
    }
    pub fn read(&self, hash: &str, limit: usize) -> Result<Vec<u8>> {
        let body = self
            .open_object(hash)?
            .ok_or_else(|| SyncError::new("cached-object-missing", 409))?;
        if body.len()? > limit as u64 {
            return Err(SyncError::new("cached-object-too-large", 413));
        }
        let mut bytes = Vec::new();
        body.take((limit as u64).saturating_add(1))
            .read_to_end(&mut bytes)?;
        if bytes.len() > limit {
            return Err(SyncError::new("cached-object-too-large", 413));
        }
        if risunest_sync_wire::hash(&bytes) != hash {
            return Err(SyncError::new("cached-object-corrupt", 409));
        }
        Ok(bytes)
    }
    pub fn dependencies(&self, payload: &ServerPayload) -> Result<Vec<String>> {
        Ok(
            crate::persistent_store::server_sync_projection::dependencies_with(payload, |hash| {
                self.read(hash, usize::MAX).map_err(|error| {
                    crate::persistent_store::StoreError::Validation {
                        message: error.code,
                    }
                })
            })?,
        )
    }
    pub fn project(
        &self,
        payload: &ServerPayload,
        dependencies: &[String],
        relations: &[String],
        scopes: Vec<String>,
    ) -> Result<ProjectedRecord> {
        let bytes =
            serde_json::to_vec(payload).map_err(|_| SyncError::new("projection-encoding", 409))?;
        self.project_bytes(&bytes, dependencies, relations, scopes)
    }

    /// What the record would be named if it were projected, for the paths that
    /// project only to check what a received record says about itself. A body
    /// that already arrived is not written back out to be compared against.
    pub fn projected_identity(
        &self,
        payload: &ServerPayload,
        dependencies: &[String],
        relations: &[String],
        scopes: Vec<String>,
    ) -> Result<ProjectedRecord> {
        let bytes =
            serde_json::to_vec(payload).map_err(|_| SyncError::new("projection-encoding", 409))?;
        self.project_into(Emission::Identity, &bytes, dependencies, relations, scopes)
    }
    /// The same record container for a body the library projection does not own,
    /// such as a device section entry.
    pub fn project_bytes(
        &self,
        bytes: &[u8],
        dependencies: &[String],
        relations: &[String],
        scopes: Vec<String>,
    ) -> Result<ProjectedRecord> {
        self.project_into(Emission::Persist, bytes, dependencies, relations, scopes)
    }

    fn project_into(
        &self,
        emission: Emission,
        bytes: &[u8],
        dependencies: &[String],
        relations: &[String],
        scopes: Vec<String>,
    ) -> Result<ProjectedRecord> {
        let local_hash = risunest_sync_wire::hash(bytes);
        let mut objects = BTreeSet::new();
        let segmented = if bytes.len() <= INLINE_PAYLOAD_BYTES {
            RecordContent::Inline {
                bytes_base64url: URL_SAFE_NO_PAD.encode(bytes),
            }
        } else {
            RecordContent::Tree {
                value: payload::build(&mut Cursor::new(bytes), |bytes| {
                    let hash = self
                        .emit(emission, bytes)
                        .map_err(|_| WireError("cache-write-failed"))?;
                    objects.insert(hash);
                    Ok(())
                })?,
            }
        };
        let object_hash = self.emit(emission, &canonical::encode(&RecordObject {
            schema: "risunest-server-record-v1".into(),
            payload: segmented,
        })?)?;
        objects.extend(dependencies.iter().cloned());
        let dependencies = objects.iter().cloned().collect::<Vec<_>>();
        let inline_dependencies = risunest_sync_wire::descriptor::inline_references(&dependencies)?;
        let (dependency_root, pages) = if inline_dependencies {
            (None, Vec::new())
        } else {
            build_reference_tree(&dependencies, false)?
        };
        for (hash, bytes) in pages {
            self.emit(emission, &bytes)?;
            objects.insert(hash);
        }
        let relations = relations
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let inline_relations = risunest_sync_wire::descriptor::inline_references(&relations)?;
        let (relation_root, pages) = if inline_relations {
            (None, Vec::new())
        } else {
            build_reference_tree(&relations, true)?
        };
        for (hash, bytes) in pages {
            self.emit(emission, &bytes)?;
            objects.insert(hash);
        }
        let descriptor = RecordDescriptor {
            object_hash: object_hash.clone(),
            dependency_root,
            relation_root,
            dependencies: if inline_dependencies {
                dependencies
            } else {
                Vec::new()
            },
            relations: if inline_relations {
                relations
            } else {
                Vec::new()
            },
            scopes,
        };
        let descriptor_hash = self.emit(emission, &descriptor.bytes()?)?;
        objects.insert(object_hash.clone());
        objects.insert(descriptor_hash.clone());
        Ok(ProjectedRecord {
            version: RecordVersion::Live {
                object_hash,
                descriptor_hash: Some(descriptor_hash),
            },
            local_hash,
            objects: objects.into_iter().collect(),
        })
    }
    pub fn restore(&self, version: &RecordVersion) -> Result<(ServerPayload, String)> {
        self.restore_with(version, |hash, limit| self.read(hash, limit))
    }
    pub fn restore_bytes(&self, version: &RecordVersion) -> Result<(Vec<u8>, String)> {
        self.restore_bytes_with(version, |hash, limit| self.read(hash, limit))
    }
    fn restore_bytes_with(
        &self,
        version: &RecordVersion,
        mut read: impl FnMut(&str, usize) -> Result<Vec<u8>>,
    ) -> Result<(Vec<u8>, String)> {
        let RecordVersion::Live { object_hash, .. } = version else {
            return Err(SyncError::new("record-is-not-live", 409));
        };
        let object: RecordObject = canonical::decode(
            &self.read(object_hash, MAX_METADATA_BYTES)?,
            MAX_METADATA_BYTES,
        )?;
        if object.schema != "risunest-server-record-v1" {
            return Err(SyncError::new("unsupported-server-record", 409));
        }
        match object.payload {
            RecordContent::Inline { bytes_base64url } => {
                let bytes = decode_inline(&bytes_base64url)?;
                let hash = risunest_sync_wire::hash(&bytes);
                Ok((bytes, hash))
            }
            RecordContent::Tree { value } => {
                let mut bytes = Vec::new();
                payload::restore(
                    &value,
                    |hash| {
                        read(hash, payload::MAX_CHUNK)
                            .map_err(|_| WireError("cached-payload-invalid"))
                    },
                    &mut bytes,
                )?;
                if bytes.len() <= INLINE_PAYLOAD_BYTES {
                    return Err(SyncError::new("noncanonical-server-payload", 409));
                }
                Ok((bytes, value.content_hash))
            }
        }
    }
    pub fn restore_with(
        &self,
        version: &RecordVersion,
        read: impl FnMut(&str, usize) -> Result<Vec<u8>>,
    ) -> Result<(ServerPayload, String)> {
        let (bytes, content_hash) = self.restore_bytes_with(version, read)?;
        let payload: ServerPayload = serde_json::from_slice(&bytes)
            .map_err(|_| SyncError::new("invalid-server-payload", 409))?;
        if serde_json::to_vec(&payload)
            .map_err(|_| SyncError::new("invalid-server-payload", 409))?
            != bytes
        {
            return Err(SyncError::new("noncanonical-server-payload", 409));
        }
        Ok((payload, content_hash))
    }
    pub fn closure(&self, version: &RecordVersion) -> Result<Vec<String>> {
        let RecordVersion::Live {
            object_hash,
            descriptor_hash: Some(descriptor_hash),
        } = version
        else {
            return Ok(version
                .object_hashes()
                .into_iter()
                .map(str::to_owned)
                .collect());
        };
        let descriptor: RecordDescriptor = canonical::decode(
            &self.read(descriptor_hash, MAX_METADATA_BYTES)?,
            MAX_METADATA_BYTES,
        )?;
        descriptor.validate()?;
        if descriptor.object_hash != *object_hash {
            return Err(SyncError::new("descriptor-record-mismatch", 409));
        }
        let mut hashes = BTreeSet::from([object_hash.clone(), descriptor_hash.clone()]);
        hashes.extend(descriptor.dependencies);
        for (root, relations) in [
            (descriptor.dependency_root, false),
            (descriptor.relation_root, true),
        ] {
            if let Some(root) = root {
                visit_reference_tree(
                    &root,
                    relations,
                    |hash| {
                        self.read(hash, MAX_METADATA_BYTES)
                            .map_err(|_| WireError("cached-descriptor-invalid"))
                    },
                    |hash, page| {
                        if page || !relations {
                            hashes.insert(hash.into());
                        }
                        Ok(())
                    },
                )?;
            }
        }
        Ok(hashes.into_iter().collect())
    }
    /// Candidate metadata/chunks belong to this record. The thousands of asset
    /// bodies reachable from an owner manifest are not a candidate inventory to
    /// stat and transmit on every small owner edit.
    pub fn base_candidates(&self, key: &str, version: &RecordVersion) -> Result<Vec<String>> {
        use crate::logical_records::{
            decode_logical_record_key, LogicalRecordEnvelope as Envelope,
            LogicalRecordLocator as Locator,
        };
        let RecordVersion::Live {
            object_hash,
            descriptor_hash: Some(descriptor_hash),
        } = version
        else {
            return Ok(Vec::new());
        };
        let descriptor: RecordDescriptor = canonical::decode(
            &self.read(descriptor_hash, MAX_METADATA_BYTES)?,
            MAX_METADATA_BYTES,
        )?;
        descriptor.validate()?;
        if descriptor.object_hash != *object_hash {
            return Err(SyncError::new("descriptor-record-mismatch", 409));
        }
        let mut hashes = BTreeSet::from([object_hash.clone(), descriptor_hash.clone()]);
        for (root, relations) in [
            (descriptor.dependency_root, false),
            (descriptor.relation_root, true),
        ] {
            if let Some(root) = root {
                visit_reference_tree(
                    &root,
                    relations,
                    |hash| {
                        self.read(hash, MAX_METADATA_BYTES)
                            .map_err(|_| WireError("cached-descriptor-invalid"))
                    },
                    |hash, page| {
                        if page {
                            hashes.insert(hash.into());
                        }
                        Ok(())
                    },
                )?;
            }
        }
        let object: RecordObject = canonical::decode(
            &self.read(object_hash, MAX_METADATA_BYTES)?,
            MAX_METADATA_BYTES,
        )?;
        if object.schema != "risunest-server-record-v1" {
            return Err(SyncError::new("unsupported-server-record", 409));
        }
        let mut pending = match object.payload {
            RecordContent::Inline { bytes_base64url } => {
                decode_inline(&bytes_base64url)?;
                Vec::new()
            }
            RecordContent::Tree { value } => vec![(value.root, 0)],
        };
        let mut seen = BTreeSet::new();
        while let Some((hash, depth)) = pending.pop() {
            if depth >= 8 || !seen.insert(hash.clone()) || seen.len() > payload::MAX_PARTS {
                return Err(SyncError::new("cached-payload-invalid", 409));
            }
            hashes.insert(hash.clone());
            let index: payload::Index =
                canonical::decode(&self.read(&hash, MAX_METADATA_BYTES)?, MAX_METADATA_BYTES)?;
            match index {
                payload::Index::Chunks { parts } => {
                    hashes.extend(parts.into_iter().map(|p| p.hash));
                }
                payload::Index::Branches { parts } => {
                    pending.extend(parts.into_iter().map(|p| (p.hash, depth + 1)));
                }
            }
        }
        let locator = decode_logical_record_key(key)
            .map_err(|_| SyncError::new("invalid-server-key", 409))?;
        if matches!(
            locator,
            Locator::Root
                | Locator::Character { .. }
                | Locator::Asset { .. }
                | Locator::Inlay { .. }
                | Locator::Cold { .. }
        ) {
            match self.restore(version)?.0.record {
                Envelope::Root { owner_heads, .. } | Envelope::Character { owner_heads, .. } => {
                    hashes.extend(owner_heads.into_iter().filter_map(|h| h.manifest_hash))
                }
                Envelope::ArchivedCharacter {
                    archive_object_hash,
                    owner_heads,
                    ..
                } => {
                    hashes.insert(archive_object_hash);
                    hashes.extend(owner_heads.into_iter().filter_map(|h| h.manifest_hash));
                }
                Envelope::Asset { object_hash, .. }
                | Envelope::Inlay { object_hash, .. }
                | Envelope::Cold { object_hash, .. } => hashes.extend(object_hash),
                _ => (),
            }
        }
        Ok(hashes.into_iter().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logical_records::LogicalRecordEnvelope;

    /// A10. The inline wrapper carries canonical unpadded base64url under its
    /// own field name, and every boundary size round trips to the same bytes.
    #[test]
    fn inline_wrapper_is_canonical_base64url_at_its_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path()).unwrap();
        for size in [0, 1, 2, 3, INLINE_PAYLOAD_BYTES - 1, INLINE_PAYLOAD_BYTES] {
            let source = (0..size).map(|i| (i % 251) as u8).collect::<Vec<_>>();
            let projected = cache.project_bytes(&source, &[], &[], Vec::new()).unwrap();
            let RecordVersion::Live { object_hash, .. } = &projected.version else {
                panic!("a projected record is live");
            };
            let encoded = cache.read(object_hash, MAX_METADATA_BYTES).unwrap();
            let wrapper: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
            let field = wrapper["payload"]["bytesBase64url"].as_str().unwrap();
            assert!(wrapper["payload"]["bytes"].is_null());
            assert!(!field.contains('=') && !field.contains('+') && !field.contains('/'));
            assert_eq!(decode_inline(field).unwrap(), source);
            assert_eq!(cache.restore_bytes(&projected.version).unwrap().0, source);
        }
    }
    /// Something that changes when a body is written a second time: the row a
    /// database body occupies, or the modification time of a file body.
    fn stored_at(cache: &Cache, hash: &str) -> String {
        use rusqlite::OptionalExtension as _;
        let row: Option<i64> = cache
            .objects()
            .unwrap()
            .query_row(
                "SELECT rowid FROM small_objects WHERE hash=?1",
                [hash],
                |r| r.get(0),
            )
            .optional()
            .unwrap();
        match row {
            Some(row) => format!("row {row}"),
            None => {
                let path = cache.cas.object_path(hash).unwrap().unwrap();
                format!("{:?}", std::fs::metadata(path).unwrap().modified().unwrap())
            }
        }
    }

    #[test]
    fn a_body_goes_to_the_store_its_size_belongs_to_and_pages_can_be_returned() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path()).unwrap();
        // Cleanup hands freed pages back to the filesystem, which only works
        // when the database was created with incremental collection.
        assert_eq!(
            cache
                .objects()
                .unwrap()
                .query_row("PRAGMA auto_vacuum", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
        let small = vec![1u8; SMALL_OBJECT_BYTES];
        let large = vec![2u8; SMALL_OBJECT_BYTES + 1];
        let small_hash = cache.put(&small).unwrap();
        let large_hash = cache.put(&large).unwrap();
        assert_eq!(
            small_object_store::size(&*cache.objects().unwrap(), &small_hash).unwrap(),
            Some(small.len() as u64)
        );
        assert_eq!(
            small_object_store::size(&*cache.objects().unwrap(), &large_hash).unwrap(),
            None
        );
        assert!(cache.cas.stat_object(&small_hash).unwrap().is_none());
        assert!(cache.cas.stat_object(&large_hash).unwrap().is_some());
        for (hash, bytes) in [(&small_hash, &small), (&large_hash, &large)] {
            assert_eq!(cache.stat_derived(hash).unwrap(), Some(bytes.len() as u64));
            assert_eq!(&cache.read(hash, bytes.len()).unwrap(), bytes);
            assert!(cache.read(hash, bytes.len() - 1).is_err());
            cache.verify(hash, || Ok(())).unwrap();
        }
    }

    #[test]
    fn a_scope_publishes_only_its_own_bodies_and_an_abandoned_one_leaves_none() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path()).unwrap();
        let body = |seed: usize| format!("synthetic small body {seed:08}").into_bytes();

        let outer = cache.begin_batch().unwrap();
        let first = cache.put(&body(1)).unwrap();
        let inner = cache.begin_batch().unwrap();
        let second = cache.put(&body(2)).unwrap();
        // Staged bodies read back exactly as written ones do, so a caller that
        // was handed an identity can use it before the group lands.
        assert_eq!(cache.read(&second, 1024).unwrap(), body(2));
        inner.commit().unwrap();
        // The inner scope published its own body and left the outer one alone.
        assert!(small_object_store::size(&*cache.objects().unwrap(), &second)
            .unwrap()
            .is_some());
        assert!(small_object_store::size(&*cache.objects().unwrap(), &first)
            .unwrap()
            .is_none());
        assert_eq!(cache.read(&first, 1024).unwrap(), body(1));
        drop(outer);
        assert!(cache.stat_derived(&first).unwrap().is_none());
        assert_eq!(cache.read(&second, 1024).unwrap(), body(2));

        // A scope that reaches the bounded group size writes what it has, so
        // an abandoned scope leaves collectable bodies rather than unbounded
        // memory. Nothing has recorded a reference to them.
        let batch = cache.begin_batch().unwrap();
        let staged = (0..BATCH_ROWS + 1)
            .map(|seed| cache.put(&body(100 + seed)).unwrap())
            .collect::<Vec<_>>();
        drop(batch);
        let durable = staged
            .iter()
            .filter(|hash| cache.stat_derived(hash).unwrap().is_some())
            .count();
        assert_eq!(durable, BATCH_ROWS);
        // Outside a scope every body is durable as soon as its identity exists.
        let alone = cache.put(&body(999)).unwrap();
        assert!(small_object_store::size(&*cache.objects().unwrap(), &alone)
            .unwrap()
            .is_some());
    }

    /// §9.4. Both sinks name the same record, object for object, and the
    /// identity sink writes nothing. It exists so that projecting a received
    /// record to check what it says about itself does not write that record
    /// back out, which for a body that arrived as a tree is every chunk of it.
    #[test]
    fn projecting_for_identity_names_what_persisting_names_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path()).unwrap();
        // Long enough to segment, with more references than either root can
        // carry inline, so chunks, both reference trees and the descriptor are
        // all emitted.
        let payload = ServerPayload {
            record: LogicalRecordEnvelope::Plugin {
                owner: "synthetic-plugin".to_owned(),
                ordinal: 0,
                value: serde_json::Value::String("x".repeat(4 * INLINE_PAYLOAD_BYTES)),
            },
            messages: None,
            derived_objects: Default::default(),
        };
        let dependencies = (0..400)
            .map(|index| risunest_sync_wire::hash(format!("dependency {index}").as_bytes()))
            .collect::<Vec<_>>();
        let relations = (0..400)
            .map(|index| risunest_sync_wire::hash(format!("relation {index}").as_bytes()))
            .collect::<Vec<_>>();
        let scopes = vec!["plugin-storage".to_owned()];

        take_put_calls();
        let named = cache
            .projected_identity(&payload, &dependencies, &relations, scopes.clone())
            .unwrap();
        assert_eq!(take_put_calls(), 0);
        for hash in &named.objects {
            assert!(cache.stat_derived(hash).unwrap().is_none());
        }

        let stored = cache
            .project(&payload, &dependencies, &relations, scopes)
            .unwrap();
        let bodies = take_put_calls();
        assert!(bodies > 0);
        assert_eq!(named.version, stored.version);
        assert_eq!(named.objects, stored.objects);
        assert_eq!(named.local_hash, stored.local_hash);
        // More than the record object and its descriptor, so the chunks and
        // the reference pages were named by both.
        assert!(named.objects.len() > 2);

        // The saving is on a record the cache already holds, which is every
        // record a validating caller is looking at: the persisting path still
        // walks each body through `put` to stat it and read it back, and the
        // identity path touches none of them.
        take_put_calls();
        assert_eq!(
            cache
                .projected_identity(&payload, &dependencies, &relations, vec!["plugin-storage".into()])
                .unwrap()
                .version,
            stored.version,
        );
        assert_eq!(take_put_calls(), 0);
        cache
            .project(&payload, &dependencies, &relations, vec!["plugin-storage".into()])
            .unwrap();
        assert_eq!(take_put_calls(), bodies);
    }

    #[test]
    fn small_records_use_two_objects_and_verified_cache_reuse() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path()).unwrap();
        let asset = cache.put(b"synthetic asset").unwrap();
        for bytes in [
            vec![],
            vec![0xff; INLINE_PAYLOAD_BYTES],
            b"synthetic record".to_vec(),
        ] {
            let record = cache
                .project_bytes(&bytes, &[asset.clone()], &["parent".into()], vec![])
                .unwrap();
            assert_eq!(record.objects.len(), 3);
            assert_eq!(cache.restore_bytes(&record.version).unwrap().0, bytes);
            assert_eq!(cache.closure(&record.version).unwrap().len(), 3);
            // A second projection of the same bytes reuses every stored body
            // instead of writing it again.
            let before = record
                .objects
                .iter()
                .map(|h| stored_at(&cache, h))
                .collect::<Vec<_>>();
            assert_eq!(
                cache
                    .project_bytes(&bytes, &[asset.clone()], &["parent".into()], vec![])
                    .unwrap()
                    .version,
                record.version
            );
            for (hash, before) in record.objects.iter().zip(before) {
                assert_eq!(stored_at(&cache, hash), before);
            }
        }
        // A stored body that no longer matches its identity is corruption, and
        // one that is gone is written again on the next request for it.
        cache
            .objects()
            .unwrap()
            .execute(
                "UPDATE small_objects SET body=?1 WHERE hash=?2",
                rusqlite::params![b"corrupted asset".to_vec(), asset],
            )
            .unwrap();
        assert!(cache.put(b"synthetic asset").is_err());
        cache
            .objects()
            .unwrap()
            .execute("DELETE FROM small_objects WHERE hash=?1", [&asset])
            .unwrap();
        assert_eq!(cache.put(b"synthetic asset").unwrap(), asset);
        assert_eq!(cache.read(&asset, 1024).unwrap(), b"synthetic asset");
        let large = vec![255; INLINE_PAYLOAD_BYTES + 1];
        let record = cache.project_bytes(&large, &[], &[], vec![]).unwrap();
        assert!(record.objects.len() > 2);
        assert_eq!(cache.restore_bytes(&record.version).unwrap().0, large);
    }

    #[test]
    fn library_source_is_checked_without_copying_into_derived_cache() {
        let root = tempfile::tempdir().unwrap();
        let native = PayloadCas::new(root.path()).unwrap();
        let object = native.prepare_bytes(b"synthetic native asset").unwrap();
        let cache = Cache::open(&root.path().join("derived"))
            .unwrap()
            .with_library(root.path())
            .unwrap();
        cache.verify(&object.content_hash, || Ok(())).unwrap();
        assert_eq!(
            cache.read(&object.content_hash, 1024).unwrap(),
            b"synthetic native asset"
        );
        assert!(cache
            .cas
            .stat_object(&object.content_hash)
            .unwrap()
            .is_none());
        std::fs::write(
            native.object_path(&object.content_hash).unwrap().unwrap(),
            b"corruption",
        )
        .unwrap();
        assert!(cache.verify(&object.content_hash, || Ok(())).is_err());
    }

    #[test]
    fn concurrent_insertion_and_malformed_inline_payloads_preserve_integrity() {
        let root = tempfile::tempdir().unwrap();
        let cache = Cache::open(root.path()).unwrap();
        std::thread::scope(|scope| {
            let workers = (0..4)
                .map(|_| scope.spawn(|| cache.put(b"concurrent synthetic").unwrap()))
                .collect::<Vec<_>>();
            for worker in workers {
                assert_eq!(
                    worker.join().unwrap(),
                    risunest_sync_wire::hash(b"concurrent synthetic")
                );
            }
        });
        for bytes_base64url in [
            // Outside the alphabet, padded, non-canonical trailing bits and a
            // body above the inline cutoff. Each is a distinct wrapper defect.
            "a+/b".to_owned(),
            format!("{}==", URL_SAFE_NO_PAD.encode(b"synthetic")),
            "YR".to_owned(),
            URL_SAFE_NO_PAD.encode(vec![0_u8; INLINE_PAYLOAD_BYTES + 1]),
        ] {
            let object_hash = cache
                .put(
                    &canonical::encode(&RecordObject {
                        schema: "risunest-server-record-v1".into(),
                        payload: RecordContent::Inline { bytes_base64url },
                    })
                    .unwrap(),
                )
                .unwrap();
            let descriptor_hash = cache
                .put(
                    &RecordDescriptor::content(object_hash.clone())
                        .bytes()
                        .unwrap(),
                )
                .unwrap();
            assert!(cache
                .restore_bytes(&RecordVersion::Live {
                    object_hash,
                    descriptor_hash: Some(descriptor_hash)
                })
                .is_err());
        }
    }

    #[test]
    #[ignore = "Explicit synthetic 1 GiB source IO measurement"]
    fn library_cas_gib_measurement() {
        let root = tempfile::tempdir().unwrap();
        let native = PayloadCas::new(root.path()).unwrap();
        let mut hashes = Vec::new();
        let mut bytes = vec![37; 4 * 1024 * 1024];
        for index in 0u32..256 {
            bytes[..4].copy_from_slice(&index.to_le_bytes());
            hashes.push(native.prepare_bytes(&bytes).unwrap().content_hash);
        }
        drop(bytes);
        for copy in [true, false] {
            let derived = tempfile::tempdir().unwrap();
            let cache = Cache::open(derived.path())
                .unwrap()
                .with_library(root.path())
                .unwrap();
            let started = std::time::Instant::now();
            for hash in &hashes {
                if copy {
                    let mut source = native.open_object(hash).unwrap().unwrap();
                    cache
                        .cas
                        .prepare_reader_expected(&mut source, hash, 4 * 1024 * 1024)
                        .unwrap();
                } else {
                    cache.verify(hash, || Ok(())).unwrap();
                }
            }
            let written: u64 = hashes
                .iter()
                .map(|h| cache.cas.stat_object(h).unwrap().unwrap_or(0))
                .sum();
            eprintln!("source_bytes=1073741824 objects=256 copy={copy} derived_body_bytes={written} elapsed_ms={}", started.elapsed().as_millis());
            assert_eq!(written, if copy { 1073741824 } else { 0 });
        }
    }

    #[test]
    #[ignore = "Explicit synthetic cache preparation measurement"]
    fn preparation_reduction_measurement() {
        let root = tempfile::tempdir().unwrap();
        let cache = Cache::open(root.path()).unwrap();
        for (pass, batched) in [(0, false), (1, true)] {
            let started = std::time::Instant::now();
            let mut objects = BTreeSet::new();
            let batch = batched.then(|| cache.begin_batch().unwrap());
            for i in 0..1000 {
                let bytes = format!("synthetic record pass={pass} {i:04}");
                objects.extend(
                    cache
                        .project_bytes(bytes.as_bytes(), &[], &[], vec![])
                        .unwrap()
                        .objects,
                );
            }
            if let Some(batch) = batch {
                batch.commit().unwrap();
            }
            let disk: u64 = objects
                .iter()
                .map(|h| cache.stat_derived(h).unwrap().unwrap())
                .sum();
            eprintln!(
                "pass={pass} batched={batched} records=1000 distinct_objects={} derived_bytes={disk} elapsed_ms={}",
                objects.len(),
                started.elapsed().as_millis()
            );
        }
    }

    #[test]
    fn candidate_inventory_excludes_a_hundred_thousand_opaque_dependencies() {
        use crate::logical_records::{encode_logical_record_key, LogicalRecordLocator};
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path()).unwrap();
        let payload = ServerPayload {
            record: LogicalRecordEnvelope::Plugin {
                owner: "synthetic-plugin".to_owned(),
                ordinal: 0,
                value: serde_json::json!("synthetic"),
            },
            messages: None,
            derived_objects: Default::default(),
        };
        let mut dependencies = (0u32..100_000)
            .map(|n| risunest_sync_wire::hash(&n.to_be_bytes()))
            .collect::<Vec<_>>();
        dependencies.sort();
        let projected = cache
            .project(&payload, &dependencies, &[], vec!["plugin-storage".into()])
            .unwrap();
        let key = encode_logical_record_key(&LogicalRecordLocator::Plugin {
            owner: "synthetic-plugin".into(),
            storage_key: "synthetic".into(),
        })
        .unwrap();
        let candidates = cache.base_candidates(&key, &projected.version).unwrap();
        assert!(candidates.len() < 2000);
        assert!(candidates
            .iter()
            .all(|hash| dependencies.binary_search(hash).is_err()));
        assert!(cache.closure(&projected.version).unwrap().len() >= 100_000);
        // Only control pages and the record's segmented payload were materialized
        // by this unit fixture; selecting candidates must not stat the raw bodies.
        assert!(dependencies.iter().take(3).all(|hash| cache
            .cas
            .stat_object(hash)
            .unwrap()
            .is_none()));
    }
    #[test]
    fn cached_record_preserves_exact_large_plugin_bytes_and_rejects_missing_chunks() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Cache::open(dir.path()).unwrap();
        let payload = ServerPayload {
            derived_objects: Default::default(),
            record: LogicalRecordEnvelope::Plugin {
                owner: "synthetic-plugin".to_owned(),
                ordinal: 0,
                value: serde_json::json!({"synthetic":"가🦀x".repeat(1_000_000),"empty":"","ordered":[null,false,1]}),
            },
            messages: None,
        };
        let projected = cache
            .project(&payload, &[], &[], vec!["plugin-storage".into()])
            .unwrap();
        let (restored, hash) = cache.restore(&projected.version).unwrap();
        assert_eq!(hash, projected.local_hash);
        assert_eq!(
            serde_json::to_vec(&restored).unwrap(),
            serde_json::to_vec(&payload).unwrap()
        );
        assert_eq!(
            cache.closure(&projected.version).unwrap(),
            projected.objects
        );
        let missing_dir = tempfile::tempdir().unwrap();
        let missing = Cache::open(missing_dir.path()).unwrap();
        assert!(missing.restore(&projected.version).is_err());
    }
}
