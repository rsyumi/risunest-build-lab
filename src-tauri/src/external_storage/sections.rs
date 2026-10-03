//! Section capture and reception for file-based remotes. Device rows become
//! codec entries here and come back the same way, so the local tables can
//! change without changing what the repository holds.
use super::contract::{Cancellation, ErrorKind, ProviderError, Result};
use crate::persistent_store::device_store::sections::{
    PreparedSectionRows, SectionRow, SectionSpoolBuilder,
};
#[cfg(not(test))]
use risunest_external_storage_format::content_identity::hash;
#[cfg(test)]
fn hash(bytes:&[u8])->[u8;32] {
    crate::persistent_store::hash_work::observe("native_external_section_content",bytes.len());
    risunest_external_storage_format::content_identity::hash(bytes)
}
use risunest_external_storage_format::{

    format::fingerprint,
    section::{
        SectionEntry, SectionKind,
        MAX_SECTION_ENTRY_BYTES, MAX_SECTION_OBJECT_BYTES,
    },
    snapshot as wire,
};
use risunest_sync_wire::head::Sequence;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write, Seek, SeekFrom},
    path::{Path, PathBuf},
};

fn corrupt(error: impl std::fmt::Display) -> ProviderError {
    ProviderError::new(ErrorKind::Corrupt).caused(&error)
}
fn transient(error: impl std::fmt::Display + 'static) -> ProviderError {
    super::packaging::transient(error)
}

/// One file the packager will carry. Section bytes live in the job spool
/// because a section changes without the library revision changing.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct SectionSource {
    pub kind: wire::CatalogEntryKind,
    pub key: String,
    pub content_sha256: String,
    pub byte_length: u64,
    pub path: PathBuf,
    pub offset: Option<u64>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct CapturedSection {
    pub kind: SectionKind,
    pub generation: Sequence,
    pub gc_floor: Sequence,
    pub max_write_clock: Sequence,
    pub content_fingerprint: [u8; 32],
    pub sources: Vec<SectionSource>,
}

struct SectionBodySpool {
    staging: tempfile::NamedTempFile,
    path: PathBuf,
    length: u64,
    offsets: BTreeMap<String, u64>,
    digest: sha2::Sha256,
}

impl SectionBodySpool {
    fn new(spool: &Path) -> Result<Self> {
        Ok(Self {
            staging: tempfile::NamedTempFile::new_in(spool).map_err(transient)?,
            path: spool.join(format!("section-bodies-{}", uuid::Uuid::new_v4())),
            length: 0,
            offsets: BTreeMap::new(),
            digest: <sha2::Sha256 as sha2::Digest>::new(),
        })
    }
    fn push(&mut self, bytes: &[u8]) -> Result<(String, PathBuf, Option<u64>)> {
        if bytes.len() > MAX_SECTION_OBJECT_BYTES { return Err(corrupt("section object limit")); }
        let digest = hex::encode(hash(bytes));
        let offset = match self.offsets.get(&digest) {
            Some(offset) => *offset,
            None => {
                let offset = self.length;
                self.staging.write_all(bytes).map_err(transient)?;
                sha2::Digest::update(&mut self.digest, bytes);
                self.length = self.length.checked_add(bytes.len() as u64)
                    .ok_or_else(|| corrupt("section spool length overflow"))?;
                self.offsets.insert(digest.clone(), offset);
                offset
            }
        };
        Ok((digest, self.path.clone(), Some(offset)))
    }
    fn finish(self) -> Result<PathBuf> {
        let digest = hex::encode(sha2::Digest::finalize(self.digest));
        let path = self.path.parent().unwrap().join(format!("section-bodies-{digest}"));
        if path.exists() {
            let mut file = crate::trust_boundary::open_regular_source(&path).map_err(transient)?;
            if file.metadata().map_err(transient)?.len() != self.length
                || hex::encode(risunest_external_storage_format::content_identity::hash_reader(&mut file, self.length)
                    .map_err(super::packaging::format_error)?) != digest {
                return Err(corrupt("section spool body differs"));
            }
            return Ok(path);
        }
        self.staging.as_file().sync_all().map_err(transient)?;
        self.staging.persist(&path).map_err(|error| transient(error.error))?;
        crate::trust_boundary::sync_directory(path.parent().unwrap()).map_err(transient)?;
        Ok(path)
    }
}

fn captured_section_fingerprint(
    kind: SectionKind,
    sources: &[SectionSource],
) -> Result<[u8; 32]> {
    #[cfg(test)]
    crate::persistent_store::hash_work::observe("native_external_section_domain",b"risunest.section-fingerprint/v1\0".len()+kind.id().len());
    let mut fingerprint = risunest_external_storage_format::format::FingerprintBuilder::new(
        &kind.fingerprint_domain(),
    );
    #[cfg(test)]
    crate::persistent_store::hash_work::observe("native_external_section_fingerprint",b"risunest.external-fingerprint/v1\0".len()+32);
    let mut previous = None;
    for source in sources.iter().filter(|source| {
        source.kind == wire::CatalogEntryKind::SectionEntry
    }) {
        if previous.as_deref() == Some(source.key.as_str()) {
            return Err(corrupt("section key appears twice"));
        }
        let digest: [u8; 32] = hex::decode(&source.content_sha256).map_err(corrupt)?
            .try_into().map_err(|_| corrupt("section entry hash is invalid"))?;
        fingerprint.push(&source.key, &digest).map_err(corrupt)?;
        #[cfg(test)]
        crate::persistent_store::hash_work::update("native_external_section_fingerprint",40+source.key.len());
        previous = Some(source.key.clone());
    }
    Ok(fingerprint.finish())
}

/// Encodes one section into spool files. `versioned` is false for a backup
/// bundle, which carries user values without the counters behind them.
pub(crate) fn capture_section(
    kind: SectionKind,
    rows: &[SectionRow],
    versioned: bool,
    generation: Sequence,
    gc_floor: Sequence,
    max_write_clock: Sequence,
    spool: &Path,
    cancel: &Cancellation,
) -> Result<CapturedSection> {
    cancel.check()?;
    fs::create_dir_all(spool).map_err(transient)?;
    if crate::trust_boundary::is_link_like(&fs::symlink_metadata(spool).map_err(transient)?) {
        return Err(corrupt("section spool is a link"));
    }
    let mut sources = Vec::new();
    let mut bodies = SectionBodySpool::new(spool)?;
    for row in rows {
        cancel.check()?;
        let entry = row.to_entry(kind, versioned).map_err(corrupt)?;
        let key = entry.key.clone();
        if let Some(body) = row.value.object_body() {
            let (digest, path, offset) = bodies.push(body)?;
            sources.push(SectionSource {
                kind: wire::CatalogEntryKind::SectionObject,
                key: format!("object/{digest}"), content_sha256: digest,
                byte_length: body.len() as u64, path, offset,
            });
        }
        let bytes = entry.encode().map_err(corrupt)?;
        let (digest, path, offset) = bodies.push(&bytes)?;
        sources.push(SectionSource {
            kind: wire::CatalogEntryKind::SectionEntry,
            key,
            content_sha256: digest,
            byte_length: bytes.len() as u64,
            path, offset,
        });
    }
    sources.sort_by(|a, b| (a.kind as u8, &a.key).cmp(&(b.kind as u8, &b.key)));
    sources.dedup_by(|a, b| {
        a.kind == wire::CatalogEntryKind::SectionObject && a.kind == b.kind && a.key == b.key
    });
    let body_path = bodies.finish()?;
    for source in &mut sources { source.path = body_path.clone(); }
    let content_fingerprint = captured_section_fingerprint(kind, &sources)?;
    Ok(CapturedSection {
        kind,
        generation,
        gc_floor,
        max_write_clock,
        content_fingerprint,
        sources,
    })
}

fn capture_prepared_section(
    prepared: &PreparedSectionRows,
    generation: Sequence,
    gc_floor: Sequence,
    max_write_clock: Sequence,
    spool: &Path,
    cancel: &Cancellation,
) -> Result<CapturedSection> {
    cancel.check()?;
    fs::create_dir_all(spool).map_err(transient)?;
    if crate::trust_boundary::is_link_like(&fs::symlink_metadata(spool).map_err(transient)?) {
        return Err(corrupt("section spool is a link"));
    }
    let kind = prepared.kind();
    let mut sources = Vec::new();
    let mut bodies = SectionBodySpool::new(spool)?;
    prepared.visit_entries_mapped(|entry, object| {
        cancel.check()?;
        if let Some(bytes) = object {
            let (digest, path, offset) = bodies.push(bytes)?;
            sources.push(SectionSource {
                kind: wire::CatalogEntryKind::SectionObject,
                key: format!("object/{digest}"), content_sha256: digest,
                byte_length: bytes.len() as u64, path, offset,
            });
        }
        let bytes = entry.encode().map_err(corrupt)?;
        let (digest, path, offset) = bodies.push(&bytes)?;
        sources.push(SectionSource {
            kind: wire::CatalogEntryKind::SectionEntry,
            key: entry.key.clone(), content_sha256: digest,
            byte_length: bytes.len() as u64, path, offset,
        });
        Ok(())
    }, preparation_error)?;
    sources.sort_by(|a, b| (a.kind as u8, &a.key).cmp(&(b.kind as u8, &b.key)));
    sources.dedup_by(|a, b| {
        a.kind == wire::CatalogEntryKind::SectionObject && a.kind == b.kind && a.key == b.key
    });
    let body_path = bodies.finish()?;
    for source in &mut sources { source.path = body_path.clone(); }
    let content_fingerprint = captured_section_fingerprint(kind, &sources)?;
    Ok(CapturedSection {
        kind, generation, gc_floor, max_write_clock,
        content_fingerprint,
        sources,
    })
}

fn device_error(error: crate::persistent_store::StoreError) -> ProviderError {
    ProviderError::new(ErrorKind::Transient).caused(&error)
}

fn preparation_error(error: crate::persistent_store::StoreError) -> ProviderError {
    match error {
        crate::persistent_store::StoreError::Validation { .. } => corrupt(error),
        _ => transient(error),
    }
}

/// What a backup connection keeps beside the library. Selected sections are
/// always present, an emptied one as a reference with no entries.
pub(crate) fn capture_backup_sections(
    store: &mut crate::persistent_store::PersistentStore,
    spool: &Path,
    cancel: &Cancellation,
) -> Result<Vec<CapturedSection>> {
    let device = store.device_store_mut().map_err(device_error)?;
    let kinds = [SectionKind::Hypa, SectionKind::LocalPlugins, SectionKind::LocalSettings];
    let prepared = device.capture_backup_sections(&kinds).map_err(device_error)?;
    capture_prepared_backup_sections(&prepared, spool, cancel)
}

pub(crate) fn capture_prepared_backup_sections(
    prepared: &[crate::persistent_store::device_store::sections::PreparedSectionRows],
    spool: &Path,
    cancel: &Cancellation,
) -> Result<Vec<CapturedSection>> {
    prepared.iter().map(|section| capture_prepared_section(
            section,
            Sequence::from(0u64),
            Sequence::from(0u64),
            Sequence::from(0u64),
            &spool.join(section.kind().id()),
            cancel,
        )).collect()
}

fn open_source_range(source: &SectionSource, cancel: &Cancellation) -> Result<std::io::Take<fs::File>> {
    cancel.check()?;
    if source.byte_length > MAX_SECTION_OBJECT_BYTES as u64
        || (source.kind == wire::CatalogEntryKind::SectionEntry
            && source.byte_length > MAX_SECTION_ENTRY_BYTES as u64) {
        return Err(corrupt("section source exceeds its codec limit"));
    }
    let metadata = fs::symlink_metadata(&source.path).map_err(transient)?;
    if !metadata.is_file() || crate::trust_boundary::is_link_like(&metadata)
        || source.offset.map_or(metadata.len() != source.byte_length, |offset| offset.checked_add(source.byte_length).is_none_or(|end| end > metadata.len())) {
        return Err(corrupt("section source is not the declared file"));
    }
    let mut file = crate::trust_boundary::open_regular_source(&source.path).map_err(transient)?;
    let length = file.metadata().map_err(transient)?.len();
    if source.offset.map_or(length != source.byte_length, |offset| offset.checked_add(source.byte_length).is_none_or(|end| end > length)) {
        return Err(corrupt("section source length changed"));
    }
    if let Some(offset) = source.offset { file.seek(SeekFrom::Start(offset)).map_err(transient)?; }
    Ok(file.take(source.byte_length))
}

pub(super) fn read_source_bytes(source: &SectionSource, cancel: &Cancellation) -> Result<Vec<u8>> {
    let mut input = open_source_range(source, cancel)?;
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        cancel.check()?;
        let count = input.read(&mut buffer).map_err(transient)?;
        if count == 0 { break; }
        bytes.extend_from_slice(&buffer[..count]);
    }
    if bytes.len() as u64 != source.byte_length { return Err(corrupt("section source length changed")); }
    Ok(bytes)
}

fn source_digest(source: &SectionSource, cancel: &Cancellation) -> Result<String> {
    use sha2::{Digest, Sha256};
    let mut input = open_source_range(source, cancel)?;
    let mut digest = Sha256::new();
    let mut length = 0;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        cancel.check()?;
        let count = input.read(&mut buffer).map_err(transient)?;
        if count == 0 { break; }
        length += count as u64;
        digest.update(&buffer[..count]);
    }
    if length != source.byte_length { return Err(corrupt("section source length changed")); }
    Ok(hex::encode(digest.finalize()))
}

fn prepare_section_source_rows(
    source: &CapturedSection,
    mut spool: SectionSpoolBuilder,
    versioned: bool,
    cancel: &Cancellation,
) -> Result<PreparedSectionRows> {
    let mut objects = BTreeMap::new();
    for file in &source.sources {
        if !crate::trust_boundary::is_lower_hex_256(&file.content_sha256) {
            return Err(corrupt("section source hash is not canonical"));
        }
        match file.kind {
            wire::CatalogEntryKind::SectionEntry => {}
            wire::CatalogEntryKind::SectionObject => {
                if file.key != format!("object/{}", file.content_sha256)
                    || file.content_sha256.len() != 64
                    || objects.insert(file.content_sha256.clone(), file).is_some()
                {
                    return Err(corrupt("section object key is invalid or duplicated"));
                }
            }
            _ => return Err(corrupt("section source has another catalog kind")),
        }
    }
    let mut used_objects = BTreeSet::new();
    for file in &source.sources {
        if file.kind != wire::CatalogEntryKind::SectionEntry { continue; }
        let bytes = read_source_bytes(file, cancel)?;
        let digest = hash(&bytes);
        if hex::encode(digest) != file.content_sha256 {
            return Err(corrupt("section entry hash differs"));
        }
        let entry = SectionEntry::decode(&bytes).map_err(corrupt)?;
        if entry.kind != source.kind || entry.key != file.key {
            return Err(corrupt("section entry names another section or key"));
        }
        let mut object = match entry.value.object_reference() {
            Some(reference) => {
                let digest = hex::encode(reference.content_sha256);
                let file = objects.get(&digest).ok_or_else(|| corrupt("section object is missing"))?;
                if file.byte_length != reference.byte_length {
                    return Err(corrupt("section object length differs"));
                }
                used_objects.insert(digest);
                Some(read_source_bytes(file, cancel)?)
            }
            None => None,
        };
        if versioned {
            let row = SectionRow::from_entry(entry, true, |_| {
                object.take().ok_or_else(|| crate::persistent_store::StoreError::Validation {
                    message: "Section object is missing".into(),
                })
            }).map_err(corrupt)?;
            spool.push(row, &file.key, &digest).map_err(preparation_error)?;
        } else {
            spool.push_backup_entry(entry, |_| {
                object.take().ok_or_else(|| crate::persistent_store::StoreError::Validation {
                    message: "Section object is missing".into(),
                })
            }).map_err(preparation_error)?;
        }
    }
    // Extra catalog objects are not installed, but corrupt ones must not be
    // hidden merely because this section has no entry that references them.
    for (digest, file) in &objects {
        if !used_objects.contains(digest) && source_digest(file, cancel)? != *digest {
            return Err(corrupt("section object hash differs"));
        }
    }
    let rows = spool.finish(&source.content_fingerprint).map_err(preparation_error)?;
    cancel.check()?;
    Ok(rows)
}

/// Validates every selected backup section before the caller changes any
/// device rows. Each returned spool is versionless and replaces only its kind.
pub(crate) fn prepare_received_backup_sections(
    sources: &[CapturedSection],
    cancel: &Cancellation,
) -> Result<Vec<PreparedSectionRows>> {
    let zero = Sequence::from(0u64);
    let mut seen = BTreeSet::new();
    let mut prepared = Vec::with_capacity(sources.len());
    for source in sources {
        cancel.check()?;
        if source.generation != zero
            || source.gc_floor != zero
            || source.max_write_clock != zero
            || !seen.insert(source.kind.id())
        {
            return Err(corrupt("backup section identity or bounds are invalid"));
        }
        prepared.push(prepare_section_source_rows(
            source,
            SectionSpoolBuilder::new_backup(source.kind).map_err(transient)?,
            false,
            cancel,
        )?);
    }
    Ok(prepared)
}

/// A decoded section as it arrived. Object bodies are resolved by content hash
/// before a row is produced, so a missing object fails the whole section.
pub(crate) fn decode_section(
    kind: SectionKind,
    entries: &[(wire::CatalogEntryKind, String, Vec<u8>)],
    expected_fingerprint: &[u8; 32],
) -> Result<Vec<SectionRow>> {
    let mut objects: BTreeMap<[u8; 32], &Vec<u8>> = BTreeMap::new();
    for (entry_kind, _, bytes) in entries {
        if *entry_kind == wire::CatalogEntryKind::SectionObject {
            objects.insert(hash(bytes), bytes);
        }
    }
    let mut fingerprints: BTreeMap<String, [u8; 32]> = BTreeMap::new();
    let mut rows = Vec::new();
    for (entry_kind, key, bytes) in entries {
        if *entry_kind != wire::CatalogEntryKind::SectionEntry {
            continue;
        }
        let entry = SectionEntry::decode(bytes).map_err(corrupt)?;
        if entry.kind != kind || entry.key != *key {
            return Err(corrupt("section entry names another section"));
        }
        if fingerprints.insert(key.clone(), hash(bytes)).is_some() {
            return Err(corrupt("section key appears twice"));
        }
        rows.push(SectionRow::from_entry(entry, false, |reference| {
            objects.get(&reference.content_sha256)
                .map(|bytes| (*bytes).clone())
                .ok_or_else(|| crate::persistent_store::StoreError::Validation {
                    message: "Section object is missing".into(),
                })
        }).map_err(corrupt)?);
    }
    #[cfg(test)]
    {
        crate::persistent_store::hash_work::observe("native_external_section_domain",b"risunest.section-fingerprint/v1\0".len()+kind.id().len());
        crate::persistent_store::hash_work::observe("native_external_section_received_fingerprint",b"risunest.external-fingerprint/v1\0".len()+32);
        for key in fingerprints.keys() {crate::persistent_store::hash_work::update("native_external_section_received_fingerprint",40+key.len());}
    }
    if fingerprint(&kind.fingerprint_domain(), &fingerprints) != *expected_fingerprint {
        return Err(corrupt("section content differs from its reference"));
    }
    Ok(rows)
}

#[cfg(test)]
#[path = "section_scale_tests.rs"]
mod scale_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ten_thousand_section_bodies_share_one_durable_file_and_exact_ranges() {
        let root = tempfile::tempdir().unwrap();
        let mut spool = SectionBodySpool::new(root.path()).unwrap();
        let mut sources = Vec::new();
        for index in 0..10_000 {
            let bytes = format!("synthetic-value-{index}").into_bytes();
            let (digest, path, offset) = spool.push(&bytes).unwrap();
            sources.push((SectionSource { kind: wire::CatalogEntryKind::SectionEntry,
                key: index.to_string(), content_sha256: digest,
                byte_length: bytes.len() as u64, path, offset }, bytes));
        }
        let first_offset = sources[0].0.offset;
        assert_eq!(spool.push(&sources[0].1).unwrap().2, first_offset);
        let path = spool.finish().unwrap();
        for (source, _) in &mut sources { source.path = path.clone(); }
        let mut identical = SectionBodySpool::new(root.path()).unwrap();
        for (_, bytes) in &sources { identical.push(bytes).unwrap(); }
        assert_eq!(identical.finish().unwrap(), path);
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 1);
        for (source, bytes) in &sources {
            assert_eq!(read_source_bytes(source, &Cancellation::default()).unwrap(), *bytes);
        }
        let mut invalid = sources[0].0.clone();
        invalid.offset = Some(u64::MAX);
        assert!(read_source_bytes(&invalid, &Cancellation::default()).is_err());
    }

    #[test]
    fn unfinished_section_spool_never_publishes_a_body_file() {
        let root = tempfile::tempdir().unwrap();
        let path = {
            let mut spool = SectionBodySpool::new(root.path()).unwrap();
            spool.push(b"interrupted").unwrap().1
        };
        assert!(!path.exists());
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 0);
    }

    use crate::persistent_store::{
        device_store::{plugin_values::PluginDeviceMutation, sections::SectionValueRow},
        PersistentStore,
    };

    fn plugin_row(key: &str, value: &str, clock: u64, writer: &str) -> SectionRow {
        SectionRow {
            key1: "plugin-a".into(),
            key2: "string".into(),
            key3: key.into(),
            value: SectionValueRow::Plugin {
                space: "string".into(),
                value: value.into(),
            },
            write_clock: Sequence::from(clock),
            writer_id: writer.into(),
        }
    }

    fn hypa_row(key: &str, clock: u64, writer: &str, dimensions: i64) -> SectionRow {
        SectionRow {
            key1: key.into(),
            key2: String::new(),
            key3: String::new(),
            value: SectionValueRow::Hypa {
                producer: "hypa-v2".into(),
                model: "text-embedding".into(),
                endpoint: None,
                preprocess_version: 1,
                dimensions,
                vector: vec![7u8; dimensions as usize * 4],
                metadata: None,
            },
            write_clock: Sequence::from(clock),
            writer_id: writer.into(),
        }
    }

    fn carried(section: &CapturedSection) -> Vec<(wire::CatalogEntryKind, String, Vec<u8>)> {
        section
            .sources
            .iter()
            .map(|source| {
                (
                    source.kind,
                    source.key.clone(),
                    read_source_bytes(source, &Cancellation::default()).expect("read section source"),
                )
            })
            .collect()
    }

    #[test]
    fn a_large_vector_becomes_an_object_and_returns_byte_identical() {
        let spool = tempfile::tempdir().expect("create spool");
        let rows = [
            hypa_row(&"a".repeat(64), 3, "writer-a", 4),
            hypa_row(&"b".repeat(64), 4, "writer-a", 2_048),
        ];
        let captured = capture_section(
            SectionKind::Hypa,
            &rows,
            true,
            Sequence::from(1u64),
            Sequence::from(0u64),
            Sequence::from(4u64),
            spool.path(),
            &Cancellation::default(),
        )
        .expect("capture hypa section");
        assert!(captured
            .sources
            .iter()
            .any(|source| source.kind == wire::CatalogEntryKind::SectionObject));
        let decoded = decode_section(
            SectionKind::Hypa,
            &carried(&captured),
            &captured.content_fingerprint,
        )
        .expect("decode hypa section");
        assert_eq!(decoded, rows);
    }

    #[test]
    fn plugin_spaces_and_device_settings_round_trip_without_being_rewritten() {
        let spool = tempfile::tempdir().expect("create spool");
        let large_json = format!("{{\n  \"pad\": \"{}\",\n  \"n\": 1.50\n}}", "p".repeat(70_000));
        let large_string = format!("{}\u{0}{}", "s".repeat(40_000), "가".repeat(10_000));
        let large_setting = format!("{{ \"history\": \"{}\" }}", "h".repeat(70_000));
        let plugin_rows = [
            SectionRow {
                key1: "provider-manager".into(),
                key2: "json".into(),
                key3: "history".into(),
                value: SectionValueRow::Plugin {
                    space: "json".into(),
                    value: large_json.clone(),
                },
                write_clock: Sequence::from(8u64),
                writer_id: "writer-a".into(),
            },
            SectionRow {
                key1: "provider-manager".into(),
                key2: "json".into(),
                key3: "settings".into(),
                value: SectionValueRow::Plugin {
                    space: "json".into(),
                    value: "{\"zeta\":1,\"alpha\":[2,3]}".into(),
                },
                write_clock: Sequence::from(9u64),
                writer_id: "writer-a".into(),
            },
            SectionRow {
                key1: "yumi-translator".into(),
                key2: "string".into(),
                key3: "cache".into(),
                value: SectionValueRow::Plugin {
                    space: "string".into(),
                    value: large_string.clone(),
                },
                write_clock: Sequence::from(7u64),
                writer_id: "writer-b".into(),
            },
            SectionRow {
                key1: "yumi-translator".into(),
                key2: "string".into(),
                key3: "token".into(),
                value: SectionValueRow::Plugin {
                    space: "string".into(),
                    value: "kept".into(),
                },
                write_clock: Sequence::from(10u64),
                writer_id: "writer-b".into(),
            },
        ];
        let plugins = capture_section(
            SectionKind::LocalPlugins,
            &plugin_rows,
            true,
            Sequence::from(2u64),
            Sequence::from(0u64),
            Sequence::from(10u64),
            spool.path(),
            &Cancellation::default(),
        )
        .expect("capture plugin section");
        assert_eq!(
            plugins.sources.iter()
                .filter(|source| source.kind == wire::CatalogEntryKind::SectionObject)
                .map(|source| source.byte_length).collect::<BTreeSet<_>>(),
            BTreeSet::from([large_json.len() as u64, large_string.len() as u64])
        );
        let decoded = decode_section(
            SectionKind::LocalPlugins,
            &carried(&plugins),
            &plugins.content_fingerprint,
        )
        .expect("decode plugin section");
        assert_eq!(decoded, plugin_rows);

        // Entry order follows the encoded key, so the permission sorts first.
        let setting_rows = [
            SectionRow {
                key1: "pluginPermission".into(),
                key2: "c".repeat(64),
                key3: "network".into(),
                value: SectionValueRow::PluginPermission { granted: true },
                write_clock: Sequence::from(0u64),
                writer_id: String::new(),
            },
            SectionRow {
                key1: "setting".into(),
                key2: "risuNestDeviceSettings".into(),
                key3: String::new(),
                value: SectionValueRow::Setting {
                    value: "{\"startup\":\"restore\"}".into(),
                },
                write_clock: Sequence::from(0u64),
                writer_id: String::new(),
            },
            SectionRow {
                key1: "setting".into(),
                key2: "risuNestUpdateSettings".into(),
                key3: String::new(),
                value: SectionValueRow::Setting { value: large_setting.clone() },
                write_clock: Sequence::from(0u64),
                writer_id: String::new(),
            },
        ];
        let settings = capture_section(
            SectionKind::LocalSettings,
            &setting_rows,
            false,
            Sequence::from(0u64),
            Sequence::from(0u64),
            Sequence::from(0u64),
            spool.path(),
            &Cancellation::default(),
        )
        .expect("capture settings section");
        let decoded = decode_section(
            SectionKind::LocalSettings,
            &carried(&settings),
            &settings.content_fingerprint,
        )
        .expect("decode settings section");
        assert_eq!(decoded, setting_rows);
    }

    /// Device values past the inline limit leave the entry as objects, so a
    /// value the plugin storage API accepts is never too large to back up. The
    /// stored text comes back byte for byte, whitespace and number spelling
    /// included.
    #[test]
    fn large_device_values_survive_external_backup_capture_prepare_and_restore() {
        let root = tempfile::tempdir().expect("create root");
        let mut source = PersistentStore::open(&root.path().join("source")).expect("open source store");
        let owner = "o".repeat(512);
        let quoted_key = "\"".repeat(512);
        let large_string = format!("{}\n\t{}", "가".repeat(25_000), "x".repeat(1_000));
        let large_json = format!("{{\n  \"zeta\": \"{}\",\n  \"alpha\": [1, 2.50, 1e2]\n}}", "y".repeat(70_000));
        let edge_string = "e".repeat(risunest_external_storage_format::section::MAX_INLINE_VALUE_BYTES);
        let large_setting = serde_json::json!({ "history": "h".repeat(70_000) });
        {
            let device = source.device_store_mut().expect("open source device store");
            device.write_plugin_device_values(&owner, &[
                PluginDeviceMutation::Set { space: "string".into(), key: quoted_key.clone(), value: large_string.clone() },
                PluginDeviceMutation::Set { space: "json".into(), key: "history".into(), value: large_json.clone() },
                PluginDeviceMutation::Set { space: "string".into(), key: "edge".into(), value: edge_string.clone() },
            ]).expect("write large plugin values");
            assert!(device.write_plugin_device_values(&owner, &[PluginDeviceMutation::Set {
                space: "string".into(), key: "k".repeat(513), value: "small".into(),
            }]).is_err());
            device.write_setting("risuNestDeviceSettings", &large_setting).expect("write large setting");
        }
        let stored_setting: String = source.device_store().unwrap().connection().query_row(
            "SELECT value FROM device_settings WHERE key='risuNestDeviceSettings'", [], |row| row.get(0),
        ).unwrap();

        let revision = source.revision().expect("read revision");
        let (lease, prepared) = source.lww_acquire_backup_capture(revision).expect("acquire backup capture");
        let captured = capture_prepared_backup_sections(
            &prepared, &root.path().join("spool"), &Cancellation::default(),
        ).expect("capture backup sections");
        source.release_revision(&lease.lease).expect("release capture lease");
        let objects = |kind| captured.iter().find(|section| section.kind == kind).unwrap().sources.iter()
            .filter(|source| source.kind == wire::CatalogEntryKind::SectionObject)
            .map(|source| source.byte_length).collect::<BTreeSet<_>>();
        assert_eq!(objects(SectionKind::LocalPlugins),
            BTreeSet::from([large_string.len() as u64, large_json.len() as u64]));
        assert_eq!(objects(SectionKind::LocalSettings), BTreeSet::from([stored_setting.len() as u64]));
        assert!(objects(SectionKind::Hypa).is_empty());

        let prepared = prepare_received_backup_sections(&captured, &Cancellation::default())
            .expect("prepare captured backup sections");
        let mut target = PersistentStore::open(&root.path().join("target")).expect("open target store");
        let stage = target.replace_begin().expect("begin replacement");
        target.replace_put_root(&stage.staging_id, &serde_json::json!({"language": "synthetic restored"}))
            .expect("stage root");
        let header = crate::persistent_store::lww::Header {
            binding_authority: target.lww_binding_authority().expect("read authority"),
            request_id: "synthetic-large-device-values".into(),
        };
        target.lww_commit_replacement_with_device_sections(
            &header, &stage.staging_id, Some(&Default::default()), &prepared.iter().collect::<Vec<_>>(),
        ).expect("restore backup with large device values");
        let device = target.device_store().expect("open target device store");
        for (space, key, expected) in [
            ("string", quoted_key.as_str(), &large_string),
            ("json", "history", &large_json),
            ("string", "edge", &edge_string),
        ] {
            assert_eq!(device.read_plugin_device_value(&owner, space, key).unwrap().as_ref(), Some(expected));
        }
        let restored_setting: String = device.connection().query_row(
            "SELECT value FROM device_settings WHERE key='risuNestDeviceSettings'", [], |row| row.get(0),
        ).unwrap();
        assert_eq!(restored_setting, stored_setting);
    }

    #[test]
    fn backup_sections_prepare_every_selected_scope_before_restore() {
        let spool = tempfile::tempdir().expect("create spool");
        let plugin = capture_section(
            SectionKind::LocalPlugins,
            &[plugin_row("token", "kept", 0, "")],
            false,
            Sequence::from(0u64),
            Sequence::from(0u64),
            Sequence::from(0u64),
            &spool.path().join("plugins"),
            &Cancellation::default(),
        )
        .expect("capture backup plugin section");
        let settings = capture_section(
            SectionKind::LocalSettings,
            &[SectionRow {
                key1: "setting".into(),
                key2: "risuNestDeviceSettings".into(),
                key3: String::new(),
                value: SectionValueRow::Setting { value: "{\"startup\":\"restore\"}".into() },
                write_clock: Sequence::from(0u64),
                writer_id: String::new(),
            }],
            false,
            Sequence::from(0u64),
            Sequence::from(0u64),
            Sequence::from(0u64),
            &spool.path().join("settings"),
            &Cancellation::default(),
        )
        .expect("capture backup settings section");
        let sources = [plugin, settings];
        let prepared = prepare_received_backup_sections(&sources, &Cancellation::default())
            .expect("prepare every backup section");
        assert_eq!(prepared.iter().map(PreparedSectionRows::kind).collect::<Vec<_>>(), [
            SectionKind::LocalPlugins,
            SectionKind::LocalSettings,
        ]);

        let mut corrupt = sources.clone();
        corrupt[1].content_fingerprint[0] ^= 1;
        assert!(prepare_received_backup_sections(&corrupt, &Cancellation::default()).is_err());
    }

    #[test]
    fn a_selected_but_empty_section_still_has_its_own_fingerprint() {
        let spool = tempfile::tempdir().expect("create spool");
        let empty = |kind| {
            capture_section(
                kind,
                &[],
                true,
                Sequence::from(1u64),
                Sequence::from(0u64),
                Sequence::from(0u64),
                spool.path(),
                &Cancellation::default(),
            )
            .expect("capture empty section")
        };
        let hypa = empty(SectionKind::Hypa);
        let plugins = empty(SectionKind::LocalPlugins);
        assert!(hypa.sources.is_empty());
        assert_ne!(hypa.content_fingerprint, plugins.content_fingerprint);
        assert!(decode_section(SectionKind::Hypa, &[], &hypa.content_fingerprint).is_ok());
        assert!(decode_section(SectionKind::Hypa, &[], &plugins.content_fingerprint).is_err());
    }

    #[test]
    fn section_content_that_differs_from_its_reference_is_refused() {
        let spool = tempfile::tempdir().expect("create spool");
        let rows = [hypa_row(&"d".repeat(64), 1, "writer-a", 4)];
        let captured = capture_section(
            SectionKind::Hypa,
            &rows,
            true,
            Sequence::from(1u64),
            Sequence::from(0u64),
            Sequence::from(1u64),
            spool.path(),
            &Cancellation::default(),
        )
        .expect("capture hypa section");
        let mut entries = carried(&captured);
        entries.clear();
        assert!(decode_section(SectionKind::Hypa, &entries, &captured.content_fingerprint).is_err());
    }
}
