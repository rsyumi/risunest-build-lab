use super::{Result, SyncError};
use crate::{
    asset_repository::PayloadCas, persistent_store::server_sync_projection::ServerPayload,
};
use risunest_sync_wire::{
    canonical,
    descriptor::{build_reference_tree, visit_reference_tree, RecordDescriptor},
    payload, RecordVersion, WireError, MAX_METADATA_BYTES,
};
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
    Inline { bytes: String },
    Tree { value: payload::Payload },
}

pub(crate) struct Cache {
    pub cas: PayloadCas,
    library: Option<PayloadCas>,
}
pub(crate) struct ProjectedRecord {
    pub version: RecordVersion,
    pub local_hash: String,
    pub objects: Vec<String>,
}
impl Cache {
    pub fn open(root: &Path) -> Result<Self> {
        std::fs::create_dir_all(root)?;
        Ok(Self {
            cas: PayloadCas::new(root)?,
            library: None,
        })
    }
    pub fn with_library(mut self, root: &Path) -> Result<Self> {
        self.library = Some(PayloadCas::new(root)?);
        Ok(self)
    }
    pub fn stat_object(&self, hash: &str) -> Result<Option<u64>> {
        if let Some(size) = self.cas.stat_object(hash)? {
            return Ok(Some(size));
        }
        Ok(match &self.library {
            Some(library) => library.stat_object(hash)?,
            None => None,
        })
    }
    pub fn open_object(&self, hash: &str) -> Result<Option<std::fs::File>> {
        if let Some(file) = self.cas.open_object(hash)? {
            return Ok(Some(file));
        }
        Ok(match &self.library {
            Some(library) => library.open_object(hash)?,
            None => None,
        })
    }
    pub fn verify(&self, hash: &str, check: impl Fn() -> Result<()>) -> Result<()> {
        use sha2::{Digest, Sha256};
        let mut file = self
            .open_object(hash)?
            .ok_or_else(|| SyncError::new("cached-object-missing", 409))?;
        let mut digest = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        loop {
            check()?;
            let size = file.read(&mut buffer)?;
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
    pub fn put(&self, bytes: &[u8]) -> Result<String> {
        let hash = risunest_sync_wire::hash(bytes);
        if self.cas.stat_object(&hash)?.is_some() {
            self.read(&hash, bytes.len())?;
            return Ok(hash);
        }
        Ok(self
            .cas
            .prepare_reader_expected(&mut Cursor::new(bytes), &hash, bytes.len() as u64)?
            .content_hash)
    }
    pub fn read(&self, hash: &str, limit: usize) -> Result<Vec<u8>> {
        let file = self
            .open_object(hash)?
            .ok_or_else(|| SyncError::new("cached-object-missing", 409))?;
        if file.metadata()?.len() > limit as u64 {
            return Err(SyncError::new("cached-object-too-large", 413));
        }
        let mut bytes = Vec::new();
        file.take((limit as u64).saturating_add(1))
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
    /// The same record container for a body the library projection does not own,
    /// such as a device section entry.
    pub fn project_bytes(
        &self,
        bytes: &[u8],
        dependencies: &[String],
        relations: &[String],
        scopes: Vec<String>,
    ) -> Result<ProjectedRecord> {
        let local_hash = risunest_sync_wire::hash(bytes);
        let mut objects = BTreeSet::new();
        let segmented = if bytes.len() <= INLINE_PAYLOAD_BYTES {
            RecordContent::Inline {
                bytes: hex::encode(bytes),
            }
        } else {
            RecordContent::Tree {
                value: payload::build(&mut Cursor::new(bytes), |bytes| {
                    let hash = self
                        .put(bytes)
                        .map_err(|_| WireError("cache-write-failed"))?;
                    objects.insert(hash);
                    Ok(())
                })?,
            }
        };
        let object_hash = self.put(&canonical::encode(&RecordObject {
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
            self.put(&bytes)?;
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
            self.put(&bytes)?;
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
        let descriptor_hash = self.put(&descriptor.bytes()?)?;
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
            RecordContent::Inline { bytes } => {
                if bytes.len() > INLINE_PAYLOAD_BYTES * 2
                    || bytes
                        .bytes()
                        .any(|b| !b.is_ascii_digit() && !(b'a'..=b'f').contains(&b))
                {
                    return Err(SyncError::new("invalid-inline-payload", 409));
                }
                let bytes = hex::decode(bytes)
                    .map_err(|_| SyncError::new("invalid-inline-payload", 409))?;
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
            RecordContent::Inline { bytes } if bytes.len() <= INLINE_PAYLOAD_BYTES * 2 => {
                Vec::new()
            }
            RecordContent::Inline { .. } => {
                return Err(SyncError::new("invalid-inline-payload", 409))
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
            let paths = record
                .objects
                .iter()
                .map(|h| cache.cas.object_path(h).unwrap().unwrap())
                .collect::<Vec<_>>();
            let modified = paths
                .iter()
                .map(|p| std::fs::metadata(p).unwrap().modified().unwrap())
                .collect::<Vec<_>>();
            assert_eq!(
                cache
                    .project_bytes(&bytes, &[asset.clone()], &["parent".into()], vec![])
                    .unwrap()
                    .version,
                record.version
            );
            for (path, before) in paths.iter().zip(modified) {
                assert_eq!(std::fs::metadata(path).unwrap().modified().unwrap(), before);
            }
        }
        let path = cache.cas.object_path(&asset).unwrap().unwrap();
        std::fs::write(&path, b"corrupted asset").unwrap();
        assert!(cache.put(b"synthetic asset").is_err());
        std::fs::remove_file(path).unwrap();
        assert_eq!(cache.put(b"synthetic asset").unwrap(), asset);
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
        for bytes in [
            "GG".to_owned(),
            "a".to_owned(),
            "00".repeat(INLINE_PAYLOAD_BYTES + 1),
        ] {
            let object_hash = cache
                .put(
                    &canonical::encode(&RecordObject {
                        schema: "risunest-server-record-v1".into(),
                        payload: RecordContent::Inline { bytes },
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
        for pass in 0..2 {
            let started = std::time::Instant::now();
            let mut objects = BTreeSet::new();
            for i in 0..1000 {
                let bytes = format!("synthetic record {i:04}");
                objects.extend(
                    cache
                        .project_bytes(bytes.as_bytes(), &[], &[], vec![])
                        .unwrap()
                        .objects,
                );
            }
            let disk: u64 = objects
                .iter()
                .map(|h| cache.cas.stat_object(h).unwrap().unwrap())
                .sum();
            eprintln!(
                "pass={pass} records=1000 distinct_objects={} derived_bytes={disk} elapsed_ms={}",
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
