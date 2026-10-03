//! Section capture and reception for file-based remotes. Device rows become
//! codec entries here and come back the same way, so the local tables can
//! change without changing what the repository holds.
use super::contract::{Cancellation, ErrorKind, ProviderError, Result};
use crate::persistent_store::device_store::{
    sections::{PreparedSectionRows, SectionCursor, SectionPublicationDisposition,
        SectionPublicationRow, SectionRow, SectionSnapshotEvent, SectionSpoolBuilder,
        SectionValueRow, TombstonePublication},
    Section,
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
        InlineOrObject, SectionEntry, SectionEntryVersion, SectionKind, SectionValue,
        MAX_SECTION_ENTRY_BYTES, MAX_SECTION_OBJECT_BYTES,
    },
    snapshot as wire,
};
use risunest_sync_wire::head::Sequence;
use rusqlite::{params, Connection, OpenFlags};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write, Seek, SeekFrom},
    path::{Path, PathBuf},
};

fn corrupt(_: impl std::fmt::Display) -> ProviderError {
    ProviderError::new(ErrorKind::Corrupt)
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

/// What a confirmed publication has to record locally for one section. Nothing
/// here is written before the remote holds the captured content, so a failed
/// publication leaves the device file as it was.
#[derive(Debug)]
pub(crate) struct SectionPublication {
    pub section: Section,
    pub publication_index_path: PathBuf,
}

/// How long a removal stays after the commit that first published it.
pub(crate) const TOMBSTONE_RETENTION_MS: u64 = 90 * 24 * 60 * 60 * 1000;

pub(crate) fn section_of(kind: SectionKind) -> Option<Section> {
    match kind {
        SectionKind::Hypa => Some(Section::Hypa),
        SectionKind::LocalPlugins => Some(Section::LocalPlugins),
        SectionKind::LocalSettings => None,
    }
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

fn publication_evidence_fingerprint(connection: &Connection) -> Result<[u8; 32]> {
    let mut fingerprint = risunest_external_storage_format::format::FingerprintBuilder::new(
        b"risunest-section-evidence-v1____",
    );
    #[cfg(test)]
    crate::persistent_store::hash_work::observe("native_external_section_evidence_fingerprint",b"risunest.external-fingerprint/v1\0".len()+32);
    let mut statement = connection.prepare(
        "SELECT key1,key2,key3,write_clock,writer_id,disposition,stamped,
            first_published_generation,first_published_at_ms
         FROM publication_rows ORDER BY key1,key2,key3",
    ).map_err(corrupt)?;
    let mut rows = statement.query([]).map_err(corrupt)?;
    while let Some(row) = rows.next().map_err(corrupt)? {
        let key = serde_json::to_string(&(
            row.get::<_, String>(0).map_err(corrupt)?,
            row.get::<_, String>(1).map_err(corrupt)?,
            row.get::<_, String>(2).map_err(corrupt)?,
        )).map_err(corrupt)?;
        let bytes = serde_json::to_vec(&(
            row.get::<_, String>(3).map_err(corrupt)?,
            row.get::<_, String>(4).map_err(corrupt)?,
            row.get::<_, i64>(5).map_err(corrupt)?,
            row.get::<_, i64>(6).map_err(corrupt)?,
            row.get::<_, Option<String>>(7).map_err(corrupt)?,
            row.get::<_, Option<i64>>(8).map_err(corrupt)?,
        )).map_err(corrupt)?;
        fingerprint.push(&key, &hash(&bytes)).map_err(corrupt)?;
        #[cfg(test)]
        crate::persistent_store::hash_work::update("native_external_section_evidence_fingerprint",40+key.len());
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
        if let (SectionValueRow::Hypa { vector, .. }, SectionValue::Hypa(value)) = (&row.value, &entry.value) {
            if matches!(value.vector, InlineOrObject::Object(_)) {
                let (digest, path, offset) = bodies.push(vector)?;
                sources.push(SectionSource {
                    kind: wire::CatalogEntryKind::SectionObject,
                    key: format!("object/{digest}"), content_sha256: digest,
                    byte_length: vector.len() as u64, path, offset,
                });
            }
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

fn device_error(_: crate::persistent_store::StoreError) -> ProviderError {
    ProviderError::new(ErrorKind::Transient)
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

struct StateSectionCapture {
    section: Section,
    kind: SectionKind,
    generation: Sequence,
    gc_floor: Sequence,
    max_write_clock: Sequence,
    participation_generation: Sequence,
    marker: TombstonePublication,
    reference: Option<wire::SectionSnapshotRef>,
    cursor: Option<SectionCursor>,
    sources: Vec<SectionSource>,
    publication_rows: u64,
    bodies: SectionBodySpool,
    publication: Option<Connection>,
    publication_stage: tempfile::NamedTempFile,
    publication_path: PathBuf,
    spool: PathBuf,
}

impl StateSectionCapture {
    fn new(
        section: Section,
        state: crate::persistent_store::device_store::sections::SectionState,
        cursor: Option<SectionCursor>,
        generation: Sequence,
        reference: Option<wire::SectionSnapshotRef>,
        at_ms: u64,
        spool: PathBuf,
    ) -> Result<Self> {
        fs::create_dir_all(&spool).map_err(transient)?;
        if crate::trust_boundary::is_link_like(&fs::symlink_metadata(&spool).map_err(transient)?) {
            return Err(corrupt("section spool is a link"));
        }
        let publication_path = spool.join("publication.sqlite");
        let at_ms = if publication_path.exists() {
            let (_, existing) = open_publication_index(&publication_path)?;
            if existing.section != section || existing.generation != generation {
                return Err(corrupt("publication index belongs to another capture"));
            }
            existing.first_published_at_ms
        } else {
            at_ms
        };
        let publication_stage = tempfile::NamedTempFile::new_in(&spool).map_err(transient)?;
        let publication = Connection::open(publication_stage.path()).map_err(transient)?;
        publication.execute_batch(
            "PRAGMA journal_mode=DELETE;
             PRAGMA synchronous=FULL;
             PRAGMA temp_store=FILE;
             PRAGMA mmap_size=0;
             CREATE TABLE publication_meta(
                singleton INTEGER PRIMARY KEY CHECK(singleton=1),
                section TEXT NOT NULL,
                participation_generation TEXT NOT NULL,
                generation TEXT NOT NULL,
                first_published_at_ms INTEGER NOT NULL,
                gc_floor TEXT NOT NULL,
                max_write_clock TEXT NOT NULL,
                content_fingerprint BLOB NOT NULL,
                evidence_fingerprint BLOB NOT NULL,
                row_count INTEGER NOT NULL
             );
             CREATE TABLE publication_rows(
                key1 TEXT NOT NULL,key2 TEXT NOT NULL,key3 TEXT NOT NULL,
                write_clock TEXT NOT NULL,writer_id TEXT NOT NULL,
                disposition INTEGER NOT NULL CHECK(disposition IN (0,1)),
                stamped INTEGER NOT NULL CHECK(stamped IN (0,1)),
                first_published_generation TEXT,
                first_published_at_ms INTEGER,
                PRIMARY KEY(key1,key2,key3),
                CHECK((first_published_generation IS NULL)=(first_published_at_ms IS NULL)),
                CHECK(disposition=0 OR (stamped=0 AND first_published_generation IS NOT NULL))
             ) WITHOUT ROWID;
             BEGIN IMMEDIATE;",
        ).map_err(transient)?;
        let kind = match section {
            Section::Hypa => SectionKind::Hypa,
            Section::LocalPlugins => SectionKind::LocalPlugins,
        };
        let inherited_floor = reference.as_ref().map(|reference| reference.gc_floor.clone())
            .unwrap_or_else(|| Sequence::from(0u64));
        Ok(Self {
            section, kind, generation: generation.clone(), gc_floor: inherited_floor,
            max_write_clock: state.max_write_clock,
            participation_generation: state.participation_generation,
            marker: TombstonePublication { generation, at_ms }, reference, cursor,
            sources: Vec::new(), publication_rows: 0,
            publication: Some(publication), publication_stage,
            publication_path, bodies: SectionBodySpool::new(&spool)?, spool,
        })
    }

    fn insert_evidence(
        &mut self,
        row: &SectionRow,
        disposition: i64,
        stamped: bool,
        first_published: Option<&TombstonePublication>,
    ) -> Result<()> {
        let at_ms = first_published.map(|marker| i64::try_from(marker.at_ms))
            .transpose().map_err(corrupt)?;
        self.publication.as_ref().expect("open publication index").execute(
            "INSERT INTO publication_rows(
                key1,key2,key3,write_clock,writer_id,disposition,stamped,
                first_published_generation,first_published_at_ms
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![row.key1, row.key2, row.key3, row.write_clock.as_str(), row.writer_id,
                disposition, stamped, first_published.map(|marker| marker.generation.as_str()), at_ms],
        ).map_err(corrupt)?;
        self.publication_rows = self.publication_rows.checked_add(1)
            .ok_or_else(|| corrupt("publication row count is exhausted"))?;
        Ok(())
    }

    fn push(&mut self, mut row: SectionRow, cancel: &Cancellation) -> Result<()> {
        cancel.check()?;
        if let Some(first_published) = row.value.first_published().cloned() {
            let reclaimable = self.reference.as_ref().zip(self.cursor.as_ref()).is_some_and(
                |(reference, cursor)| cursor.applied_generation >= reference.generation
                    && first_published.generation <= reference.generation
                    && self.marker.at_ms.saturating_sub(first_published.at_ms) >= TOMBSTONE_RETENTION_MS,
            );
            if reclaimable {
                self.gc_floor = self.gc_floor.clone().max(first_published.generation.clone());
                self.insert_evidence(&row, 1, false, Some(&first_published))?;
                return Ok(());
            }
        }
        let stamped = matches!(row.value, SectionValueRow::Tombstone { first_published: None });
        if stamped {
            if self.marker.generation == Sequence::from(0u64) { return Ok(()); }
            row.value = SectionValueRow::Tombstone {
                first_published: Some(self.marker.clone()),
            };
        }
        let entry = row.to_entry(self.kind, true).map_err(corrupt)?;
        if let (SectionValueRow::Hypa { vector, .. }, SectionValue::Hypa(value)) = (&row.value, &entry.value) {
            if matches!(value.vector, InlineOrObject::Object(_)) {
                let (digest, path, offset) = self.bodies.push(vector)?;
                self.sources.push(SectionSource {
                    kind: wire::CatalogEntryKind::SectionObject,
                    key: format!("object/{digest}"), content_sha256: digest,
                    byte_length: vector.len() as u64, path, offset,
                });
            }
        }
        let bytes = entry.encode().map_err(corrupt)?;
        let (digest, path, offset) = self.bodies.push(&bytes)?;
        self.sources.push(SectionSource {
            kind: wire::CatalogEntryKind::SectionEntry,
            key: entry.key, content_sha256: digest, byte_length: bytes.len() as u64, path, offset,
        });
        let stamped_marker = stamped.then(|| self.marker.clone());
        self.insert_evidence(&row, 0, stamped, stamped_marker.as_ref())
    }

    fn finish(mut self) -> Result<(CapturedSection, SectionPublication)> {
        let row_count = i64::try_from(self.publication_rows).map_err(corrupt)?;
        let at_ms = i64::try_from(self.marker.at_ms).map_err(corrupt)?;
        self.sources.sort_by(|a, b| (a.kind as u8, &a.key).cmp(&(b.kind as u8, &b.key)));
        self.sources.dedup_by(|a, b| {
            a.kind == wire::CatalogEntryKind::SectionObject && a.kind == b.kind && a.key == b.key
        });
        let content_fingerprint = captured_section_fingerprint(self.kind, &self.sources)?;
        let publication = self.publication.take().expect("open publication index");
        let evidence_fingerprint = publication_evidence_fingerprint(&publication)?;
        publication.execute(
            "INSERT INTO publication_meta(
                singleton,section,participation_generation,generation,first_published_at_ms,
                gc_floor,max_write_clock,content_fingerprint,evidence_fingerprint,row_count
             ) VALUES (1,?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![self.section.as_str(), self.participation_generation.as_str(),
                self.marker.generation.as_str(), at_ms, self.gc_floor.as_str(),
                self.max_write_clock.as_str(), content_fingerprint.as_slice(),
                evidence_fingerprint.as_slice(), row_count],
        ).map_err(transient)?;
        let body_path = self.bodies.finish()?;
        for source in &mut self.sources { source.path = body_path.clone(); }
        publication.execute_batch("COMMIT;").map_err(transient)?;
        drop(publication);
        self.publication_stage.as_file().sync_all().map_err(transient)?;
        let expected = PublicationMeta {
            section: self.section,
            participation_generation: self.participation_generation.clone(),
            generation: self.generation.clone(),
            first_published_at_ms: self.marker.at_ms,
            gc_floor: self.gc_floor.clone(),
            max_write_clock: self.max_write_clock.clone(),
            content_fingerprint,
            evidence_fingerprint,
        };
        if self.publication_path.exists() {
            let (_, existing) = open_publication_index(&self.publication_path)?;
            if existing != expected {
                return Err(corrupt("publication index differs from this capture"));
            }
        } else {
            self.publication_stage.persist(&self.publication_path)
                .map_err(|error| transient(error.error))?;
        }
        crate::trust_boundary::sync_directory(&self.spool).map_err(transient)?;
        Ok((CapturedSection {
            kind: self.kind,
            generation: self.generation,
            gc_floor: self.gc_floor,
            max_write_clock: self.max_write_clock,
            content_fingerprint,
            sources: self.sources,
        }, SectionPublication {
            section: self.section,
            publication_index_path: self.publication_path,
        }))
    }
}

/// What a synchronization connection publishes. Only a participating section
/// is captured; the rest keep whatever reference the observed state carried.
pub(crate) fn capture_state_sections(
    store: &mut crate::persistent_store::PersistentStore,
    generation: &Sequence,
    parent: &BTreeMap<String, wire::SectionSnapshotRef>,
    connection_id: &str,
    library_lineage: &str,
    spool: &Path,
    cancel: &Cancellation,
) -> Result<(Vec<CapturedSection>, Vec<SectionPublication>)> {
    let at_ms = u64::try_from(
        crate::persistent_store::device_store::now_ms().map_err(device_error)?,
    )
    .map_err(corrupt)?;
    let mut captured = Vec::new();
    let mut publications = Vec::new();
    let mut current: Option<StateSectionCapture> = None;
    let mut behind_floor = None;
    let device = store.device_store_mut().map_err(device_error)?;
    let visit = device.visit_participating_section_snapshot_mapped(
        connection_id,
        library_lineage,
        |event| {
            match event {
                SectionSnapshotEvent::Section { section, state, cursor } => {
                    if let Some(previous) = current.take() {
                        let (section, publication) = previous.finish()?;
                        captured.push(section);
                        publications.push(publication);
                    }
                    let kind = match section {
                        Section::Hypa => SectionKind::Hypa,
                        Section::LocalPlugins => SectionKind::LocalPlugins,
                    };
                    let reference = parent.get(kind.id()).cloned();
                    let applied = cursor.as_ref().map(|cursor| cursor.applied_generation.clone())
                        .unwrap_or_else(|| Sequence::from(0u64));
                    if reference.as_ref().is_some_and(|reference| reference.gc_floor > applied) {
                        behind_floor = Some(section);
                        return Err(transient("this device is behind the remote section floor"));
                    }
                    current = Some(StateSectionCapture::new(
                        section, state, cursor, generation.clone(), reference, at_ms,
                        spool.join(kind.id()),
                    )?);
                }
                SectionSnapshotEvent::Row { section, row } => {
                    let capture = current.as_mut()
                        .filter(|capture| capture.section == section)
                        .ok_or_else(|| corrupt("section snapshot row has no header"))?;
                    capture.push(row, cancel)?;
                }
            }
            Ok(())
        },
        preparation_error,
    );
    if let Some(section) = behind_floor {
        device.forget_section_cursor(connection_id, library_lineage, section)
            .map_err(device_error)?;
        return Err(transient("this device is behind the remote section floor"));
    }
    visit?;
    if let Some(previous) = current.take() {
        let (section, publication) = previous.finish()?;
        captured.push(section);
        publications.push(publication);
    }
    Ok((captured, publications))
}

#[derive(PartialEq, Eq)]
struct PublicationMeta {
    section: Section,
    participation_generation: Sequence,
    generation: Sequence,
    first_published_at_ms: u64,
    gc_floor: Sequence,
    max_write_clock: Sequence,
    content_fingerprint: [u8; 32],
    evidence_fingerprint: [u8; 32],
}

fn stored_sequence(value: String) -> Result<Sequence> {
    value.try_into().map_err(corrupt)
}

fn open_publication_index(path: &Path) -> Result<(Connection, PublicationMeta)> {
    let metadata = fs::symlink_metadata(path).map_err(transient)?;
    if !metadata.is_file() || crate::trust_boundary::is_link_like(&metadata) {
        return Err(corrupt("publication index is not a regular file"));
    }
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ).map_err(corrupt)?;
    connection.execute_batch("PRAGMA query_only=ON; PRAGMA mmap_size=0; BEGIN;")
        .map_err(corrupt)?;
    let (section, participation_generation, generation, first_at_ms, gc_floor,
        max_write_clock, content_fingerprint, evidence_fingerprint, expected_rows):
        (String, String, String, i64, String, String, Vec<u8>, Vec<u8>, i64) =
        connection.query_row(
            "SELECT section,participation_generation,generation,first_published_at_ms,
                gc_floor,max_write_clock,content_fingerprint,evidence_fingerprint,row_count
             FROM publication_meta WHERE singleton=1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?,
                row.get(4)?, row.get(5)?, row.get(6)?, row.get(7)?, row.get(8)?)),
        ).map_err(corrupt)?;
    let section = crate::persistent_store::device_store::sections::section_from_id(&section)
        .ok_or_else(|| corrupt("publication index names an unknown section"))?;
    let actual_rows: i64 = connection.query_row(
        "SELECT count(*) FROM publication_rows", [], |row| row.get(0),
    ).map_err(corrupt)?;
    if expected_rows < 0 || actual_rows != expected_rows {
        return Err(corrupt("publication index row count differs"));
    }
    let content_fingerprint = content_fingerprint.try_into()
        .map_err(|_| corrupt("publication content fingerprint is invalid"))?;
    let stored_evidence_fingerprint: [u8; 32] = evidence_fingerprint.try_into()
        .map_err(|_| corrupt("publication evidence fingerprint is invalid"))?;
    let evidence_fingerprint = publication_evidence_fingerprint(&connection)?;
    if evidence_fingerprint != stored_evidence_fingerprint {
        return Err(corrupt("publication evidence fingerprint differs"));
    }
    Ok((connection, PublicationMeta {
        section,
        participation_generation: stored_sequence(participation_generation)?,
        generation: stored_sequence(generation)?,
        first_published_at_ms: u64::try_from(first_at_ms).map_err(corrupt)?,
        gc_floor: stored_sequence(gc_floor)?,
        max_write_clock: stored_sequence(max_write_clock)?,
        content_fingerprint,
        evidence_fingerprint,
    }))
}

pub(crate) fn load_prepared_section_publications(
    sections_spool: &Path,
) -> Result<Vec<SectionPublication>> {
    let mut publications = Vec::new();
    for (kind, section) in [
        (SectionKind::Hypa, Section::Hypa),
        (SectionKind::LocalPlugins, Section::LocalPlugins),
    ] {
        let path = sections_spool.join(kind.id()).join("publication.sqlite");
        if !path.exists() { continue; }
        let (_, metadata) = open_publication_index(&path)?;
        if metadata.section != section {
            return Err(corrupt("publication index is under the wrong section"));
        }
        publications.push(SectionPublication { section, publication_index_path: path });
    }
    Ok(publications)
}

fn publication_row(
    row: &rusqlite::Row<'_>,
    section: Section,
) -> crate::persistent_store::StoreResult<SectionPublicationRow> {
    let key: (String, String, String) = (row.get(0)?, row.get(1)?, row.get(2)?);
    let valid_key = match section {
        Section::Hypa => key.1.is_empty() && key.2.is_empty()
            && risunest_external_storage_format::section::hypa_entry_key(&key.0).is_ok(),
        Section::LocalPlugins => risunest_external_storage_format::section::local_plugin_entry_key(
            &key.0, &key.1, &key.2,
        ).is_ok(),
    };
    if !valid_key {
        return Err(crate::persistent_store::StoreError::Validation {
            message: "Publication index key is invalid".into(),
        });
    }
    let write_clock: String = row.get(3)?;
    let writer_id: String = row.get(4)?;
    let write_clock = write_clock.try_into().map_err(|_| crate::persistent_store::StoreError::Validation {
        message: "Publication index write clock is invalid".into(),
    })?;
    if writer_id.is_empty() || writer_id.len() > 1024 {
        return Err(crate::persistent_store::StoreError::Validation {
            message: "Publication index writer is invalid".into(),
        });
    }
    let disposition: i64 = row.get(5)?;
    let stamped: i64 = row.get(6)?;
    let first_generation: Option<String> = row.get(7)?;
    let first_at_ms: Option<i64> = row.get(8)?;
    let first_published = match (first_generation, first_at_ms) {
        (Some(generation), Some(at_ms)) => Some(TombstonePublication {
            generation: generation.try_into().map_err(|_| crate::persistent_store::StoreError::Validation {
                message: "Publication removal generation is invalid".into(),
            })?,
            at_ms: u64::try_from(at_ms).map_err(|_| crate::persistent_store::StoreError::Validation {
                message: "Publication removal time is invalid".into(),
            })?,
        }),
        (None, None) => None,
        _ => return Err(crate::persistent_store::StoreError::Validation {
            message: "Publication removal marker is incomplete".into(),
        }),
    };
    let disposition = match (disposition, stamped, first_published) {
        (0, 0, None) => SectionPublicationDisposition::Published { first_published: None },
        (0, 1, Some(marker)) => SectionPublicationDisposition::Published {
            first_published: Some(marker),
        },
        (1, 0, Some(marker)) => SectionPublicationDisposition::Reclaimed {
            first_published: marker,
        },
        _ => return Err(crate::persistent_store::StoreError::Validation {
            message: "Publication row disposition is invalid".into(),
        }),
    };
    Ok(SectionPublicationRow {
        key,
        version: SectionEntryVersion { write_clock, writer_id },
        disposition,
    })
}

pub(crate) fn note_prepared_section_published(
    device: &mut crate::persistent_store::device_store::DeviceStore,
    publication: &SectionPublication,
    connection_id: &str,
    library_lineage: &str,
    confirmed: &wire::SectionSnapshotRef,
) -> Result<bool> {
    let (connection, metadata) = open_publication_index(&publication.publication_index_path)?;
    // Packaging keeps an older generation when the exact section content was
    // already remote. Its fingerprint and bounds still have to match.
    if metadata.section != publication.section
        || section_of(confirmed.kind) != Some(publication.section)
        || confirmed.generation > metadata.generation
        || metadata.gc_floor != confirmed.gc_floor
        || metadata.max_write_clock != confirmed.max_write_clock
        || metadata.content_fingerprint != confirmed.content_fingerprint
    {
        return Err(corrupt("confirmed section differs from its publication index"));
    }
    let cursor = SectionCursor {
        applied_generation: confirmed.generation.clone(),
        applied_gc_floor: confirmed.gc_floor.clone(),
        observed_max_write_clock: confirmed.max_write_clock.clone(),
    };
    let mut statement = connection.prepare(
        "SELECT key1,key2,key3,write_clock,writer_id,disposition,stamped,
            first_published_generation,first_published_at_ms
         FROM publication_rows ORDER BY key1,key2,key3",
    ).map_err(corrupt)?;
    let mut rows = statement.query([]).map_err(corrupt)?;
    let evidence = std::iter::from_fn(|| match rows.next() {
        Ok(Some(row)) => Some(publication_row(row, publication.section)),
        Ok(None) => None,
        Err(error) => Some(Err(error.into())),
    });
    device.note_spooled_section_published(
        publication.section,
        &metadata.participation_generation,
        &TombstonePublication {
            generation: metadata.generation,
            at_ms: metadata.first_published_at_ms,
        },
        &metadata.gc_floor,
        (connection_id, library_lineage, &cursor),
        evidence,
    ).map_err(device_error)
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
        let mut object = match &entry.value {
            SectionValue::Hypa(value) => match &value.vector {
                InlineOrObject::Object(reference) => {
                    let digest = hex::encode(reference.content_sha256);
                    let file = objects.get(&digest).ok_or_else(|| corrupt("section object is missing"))?;
                    if file.byte_length != reference.byte_length {
                        return Err(corrupt("section object length differs"));
                    }
                    used_objects.insert(digest);
                    Some(read_source_bytes(file, cancel)?)
                }
                _ => None,
            },
            _ => None,
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
        device_store::plugin_values::PluginDeviceMutation, PersistentStore,
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

    fn plugin_tombstone(
        key: &str,
        clock: u64,
        writer: &str,
        marker: Option<(u64, u64)>,
    ) -> SectionRow {
        SectionRow {
            value: SectionValueRow::Tombstone {
                first_published: marker.map(|(generation, at_ms)| TombstonePublication {
                    generation: Sequence::from(generation),
                    at_ms,
                }),
            },
            ..plugin_row(key, "", clock, writer)
        }
    }

    /// Every plugin key the device file still holds, with the removals marked.
    fn held_plugin_keys(store: &mut PersistentStore) -> Vec<(String, bool)> {
        store
            .device_store_mut()
            .expect("open device store")
            .read_section_rows(Section::LocalPlugins)
            .expect("read section rows")
            .into_iter()
            .map(|row| (row.key3, row.value.is_tombstone()))
            .collect()
    }
    fn assert_retained_removal(store: &mut PersistentStore, expected: SectionRow) {
        let rows = store.device_store_mut().unwrap().read_section_rows(Section::LocalPlugins).unwrap();
        assert_eq!(rows.iter().find(|row| row.key1 == expected.key1 && row.key2 == expected.key2 && row.key3 == expected.key3), Some(&expected));
    }

    fn joined(store: &mut PersistentStore, generation: u64, observed: u64) {
        store
            .device_store_mut()
            .expect("open device store")
            .write_section_cursor(
                "connection",
                "library",
                Section::LocalPlugins,
                &SectionCursor {
                    applied_generation: Sequence::from(generation),
                    applied_gc_floor: Sequence::from(0u64),
                    observed_max_write_clock: Sequence::from(observed),
                },
            )
            .expect("record what this lineage carried");
    }

    fn participating_plugin_store(root: &Path) -> PersistentStore {
        let mut store = PersistentStore::open(root).expect("open persistent store");
        let device = store.device_store_mut().expect("open device store");
        device
            .set_section_participating(Section::LocalPlugins, true)
            .expect("take part in the plugin section");
        store
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

    fn confirm_publication(
        store: &mut PersistentStore,
        publication: &SectionPublication,
        captured: &CapturedSection,
    ) {
        let mut confirmed = section_reference(
            captured.generation.as_str().parse().expect("test generation fits u64"),
            captured.gc_floor.as_str().parse().expect("test floor fits u64"),
        );
        confirmed.kind = captured.kind;
        confirmed.max_write_clock = captured.max_write_clock.clone();
        confirmed.content_fingerprint = captured.content_fingerprint;
        assert!(note_prepared_section_published(
            store.device_store_mut().expect("open device store"),
            publication,
            "connection",
            "library",
            &confirmed,
        ).expect("record the confirmed publication"));
    }

    fn captured_for<'a>(
        captured: &'a [CapturedSection],
        publication: &SectionPublication,
    ) -> &'a CapturedSection {
        captured.iter().find(|captured| section_of(captured.kind) == Some(publication.section))
            .expect("captured publication section")
    }

    fn publication_evidence_count(publication: &SectionPublication, disposition: i64) -> i64 {
        let (connection, _) = open_publication_index(&publication.publication_index_path)
            .expect("open publication index");
        connection.query_row(
            "SELECT count(*) FROM publication_rows WHERE disposition=?1",
            [disposition],
            |row| row.get(0),
        ).expect("count publication evidence")
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
        let plugin_rows = [
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

    fn published_plugin_values(section: &CapturedSection) -> Vec<(String, Option<String>)> {
        decode_section(
            SectionKind::LocalPlugins,
            &carried(section),
            &section.content_fingerprint,
        )
        .expect("decode the published section")
        .into_iter()
        .map(|row| {
            (
                row.key3,
                match row.value {
                    SectionValueRow::Plugin { value, .. } => Some(value),
                    _ => None,
                },
            )
        })
        .collect()
    }

    /// A confirmed publication records the versions it carried, so the next
    /// cycle stops offering the same section. A write that landed after the
    /// capture is outside that record and still owes a publication.
    #[test]
    fn a_recorded_publication_does_not_make_every_cycle_capture_again() {
        let spool = tempfile::tempdir().expect("create spool");
        let root = tempfile::tempdir().expect("create store root");
        let mut store = PersistentStore::open(root.path()).expect("open persistent store");
        {
            let device = store.device_store_mut().expect("open device store");
            device
                .set_section_participating(Section::LocalPlugins, true)
                .expect("take part in the plugin section");
            device
                .write_plugin_device_values(
                    "plugin-a",
                    &[PluginDeviceMutation::Set {
                        space: "string".into(),
                        key: "mine".into(),
                        value: "local".into(),
                    }],
                )
                .expect("write a local plugin value");
            device
                .write_section_cursor(
                    "connection",
                    "library",
                    Section::LocalPlugins,
                    &SectionCursor {
                        applied_generation: Sequence::from(3u64),
                        applied_gc_floor: Sequence::from(0u64),
                        observed_max_write_clock: Sequence::from(1u64),
                    },
                )
                .expect("record what this lineage carried");
        }
        let (captured, publications) = capture_state_sections(
            &mut store,
            &Sequence::from(4u64),
            &BTreeMap::new(),
            "connection",
            "library",
            &spool.path().join("published"),
            &Cancellation::default(),
        )
        .expect("capture the state sections");
        assert_eq!(publications.iter().map(|publication| publication.section).collect::<Vec<_>>(), vec![Section::Hypa, Section::LocalPlugins]);

        assert!(store.device_store_mut().expect("open device store")
            .sections_await_publication("connection", "library")
            .expect("read awaiting publication"));
        for publication in &publications {
            confirm_publication(&mut store, publication, captured_for(&captured, publication));
        }
        let device = store.device_store_mut().expect("open device store");
        assert!(!device
            .sections_await_publication("connection", "library")
            .expect("read awaiting publication"));

        device
            .write_plugin_device_values(
                "plugin-a",
                &[PluginDeviceMutation::Set {
                    space: "string".into(),
                    key: "mine".into(),
                    value: "changed".into(),
                }],
            )
            .expect("write over the published value");
        assert!(device
            .sections_await_publication("connection", "library")
            .expect("read awaiting publication"));
    }

    #[test]
    fn an_identical_remote_section_may_keep_its_earlier_generation() {
        let spool = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let mut store = participating_plugin_store(root.path());
        store.device_store_mut().unwrap().write_plugin_device_values(
            "plugin-a",
            &[PluginDeviceMutation::Set {
                space: "string".into(),
                key: "same".into(),
                value: "value".into(),
            }],
        ).unwrap();
        let (captured, publications) = capture_state_sections(
            &mut store,
            &Sequence::from(4u64),
            &BTreeMap::new(),
            "connection",
            "library",
            spool.path(),
            &Cancellation::default(),
        ).unwrap();
        let publication = publications.iter()
            .find(|publication| publication.section == Section::LocalPlugins).unwrap();
        let captured = captured_for(&captured, publication);
        let mut confirmed = section_reference(3, 0);
        confirmed.kind = captured.kind;
        confirmed.max_write_clock = captured.max_write_clock.clone();
        confirmed.content_fingerprint = captured.content_fingerprint;
        assert!(note_prepared_section_published(
            store.device_store_mut().unwrap(),
            publication,
            "connection",
            "library",
            &confirmed,
        ).unwrap());
        assert!(!store.device_store_mut().unwrap()
            .sections_await_publication("connection", "library").unwrap());
    }

    #[test]
    fn a_same_job_recapture_reuses_but_never_replaces_publication_evidence() {
        let spool = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let mut store = participating_plugin_store(root.path());
        store.device_store_mut().unwrap().write_plugin_device_values(
            "plugin-a",
            &[PluginDeviceMutation::Set {
                space: "string".into(),
                key: "same".into(),
                value: "first".into(),
            }],
        ).unwrap();
        let capture = |store: &mut PersistentStore| capture_state_sections(
            store,
            &Sequence::from(4u64),
            &BTreeMap::new(),
            "connection",
            "library",
            spool.path(),
            &Cancellation::default(),
        );
        let (_, first) = capture(&mut store).unwrap();
        let index = first.iter().find(|publication| {
            publication.section == Section::LocalPlugins
        }).unwrap().publication_index_path.clone();
        let original = fs::read(&index).unwrap();
        let mut permissions = fs::metadata(&index).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&index, permissions).unwrap();
        capture(&mut store).expect("identical recapture reuses immutable evidence");

        store.device_store_mut().unwrap().write_plugin_device_values(
            "plugin-a",
            &[PluginDeviceMutation::Set {
                space: "string".into(),
                key: "same".into(),
                value: "changed".into(),
            }],
        ).unwrap();
        assert_eq!(capture(&mut store).unwrap_err().kind, ErrorKind::Corrupt);
        assert_eq!(fs::read(&index).unwrap(), original);
        let mut permissions = fs::metadata(&index).unwrap().permissions();
        permissions.set_readonly(false);
        fs::set_permissions(&index, permissions).unwrap();
    }

    fn removal_marker(store: &mut PersistentStore, key: &str) -> Option<TombstonePublication> {
        store
            .device_store_mut()
            .expect("open device store")
            .read_section_rows(Section::LocalPlugins)
            .expect("read section rows")
            .into_iter()
            .find(|row| row.key3 == key)
            .and_then(|row| match row.value {
                SectionValueRow::Tombstone { first_published } => first_published,
                _ => None,
            })
    }

    /// A removal this device has not published yet takes the commit number and
    /// the time of the publication that carries it, and the device file records
    /// the same marker once that publication is confirmed. A removal that
    /// already carries one keeps it, so every device judges its age alike.
    #[test]
    fn a_removal_takes_the_marker_of_the_publication_that_carries_it() {
        let spool = tempfile::tempdir().expect("create spool");
        let root = tempfile::tempdir().expect("create store root");
        let mut store = PersistentStore::open(root.path()).expect("open persistent store");
        {
            let device = store.device_store_mut().expect("open device store");
            device
                .set_section_participating(Section::LocalPlugins, true)
                .expect("take part in the plugin section");
            device
                .write_plugin_device_values(
                    "plugin-a",
                    &[PluginDeviceMutation::Set {
                        space: "string".into(),
                        key: "gone".into(),
                        value: "value".into(),
                    }],
                )
                .expect("write a local plugin value");
            device
                .write_plugin_device_values(
                    "plugin-a",
                    &[PluginDeviceMutation::Delete {
                        space: "string".into(),
                        key: "gone".into(),
                    }],
                )
                .expect("remove the local plugin value");
        }
        assert!(removal_marker(&mut store, "gone").is_none());

        let (captured, publications) = capture_state_sections(
            &mut store,
            &Sequence::from(4u64),
            &BTreeMap::new(),
            "connection",
            "library",
            &spool.path().join("published"),
            &Cancellation::default(),
        )
        .expect("capture the state sections");
        let plugins = captured
            .iter()
            .find(|section| section.kind == SectionKind::LocalPlugins)
            .expect("published plugin section");
        let decoded = decode_section(
            SectionKind::LocalPlugins,
            &carried(plugins),
            &plugins.content_fingerprint,
        )
        .expect("decode the published section");
        let SectionValueRow::Tombstone { first_published } = &decoded[0].value else {
            panic!("the removal did not travel as one");
        };
        let carried_marker = first_published.clone().expect("a published marker");
        assert_eq!(carried_marker.generation, Sequence::from(4u64));
        assert!(carried_marker.at_ms > 0);
        // Nothing is recorded before the remote holds the capture.
        assert!(removal_marker(&mut store, "gone").is_none());

        for publication in &publications {
            confirm_publication(&mut store, publication, captured_for(&captured, publication));
        }
        assert_eq!(removal_marker(&mut store, "gone"), Some(carried_marker));

        let (republished, _) = capture_state_sections(
            &mut store,
            &Sequence::from(5u64),
            &BTreeMap::new(),
            "connection",
            "library",
            &spool.path().join("republished"),
            &Cancellation::default(),
        )
        .expect("capture the state sections again");
        let plugins = republished
            .iter()
            .find(|section| section.kind == SectionKind::LocalPlugins)
            .expect("published plugin section");
        let decoded = decode_section(
            SectionKind::LocalPlugins,
            &carried(plugins),
            &plugins.content_fingerprint,
        )
        .expect("decode the republished section");
        assert_eq!(decoded[0].value, removal_marker_row(&mut store, "gone"));
    }

    fn removal_marker_row(store: &mut PersistentStore, key: &str) -> SectionValueRow {
        SectionValueRow::Tombstone {
            first_published: removal_marker(store, key),
        }
    }

    fn section_reference(generation: u64, gc_floor: u64) -> wire::SectionSnapshotRef {
        wire::SectionSnapshotRef {
            kind: SectionKind::LocalPlugins,
            codec: risunest_external_storage_format::section::SECTION_CODEC.into(),
            generation: Sequence::from(generation),
            gc_floor: Sequence::from(gc_floor),
            max_write_clock: Sequence::from(0u64),
            entries_root: wire::StoredObject {
                header: wire::PublicObjectHeader::new(
                    "repository".into(),
                    "catalog-synthetic".into(),
                    wire::ObjectRole::Catalog,
                    1,
                )
                .expect("a synthetic object header"),
                locator: wire::WireLocator {
                    connection_identity: "connection".into(),
                    collection: None,
                    object: "catalog-synthetic".into(),
                },
                ciphertext_length: 1,
                ciphertext_sha256: [0; 32],
                plaintext_length: 1,
                plaintext_sha256: [0; 32],
            },
            content_fingerprint: [0; 32],
        }
    }

    fn parent_sections(generation: u64, gc_floor: u64) -> BTreeMap<String, wire::SectionSnapshotRef> {
        BTreeMap::from([(
            SectionKind::LocalPlugins.id().to_owned(),
            section_reference(generation, gc_floor),
        )])
    }

    /// A device whose cursor is below the remote floor has not seen the
    /// removals the floor covers, so it may not publish an increment over them,
    /// and it forgets how far it applied that lineage.
    #[test]
    fn a_device_behind_the_remote_floor_does_not_publish_an_increment() {
        let spool = tempfile::tempdir().expect("create spool");
        let root = tempfile::tempdir().expect("create store root");
        let mut store = participating_plugin_store(root.path());
        {
            let device = store.device_store_mut().expect("open device store");
            device
                .write_plugin_device_values(
                    "plugin-a",
                    &[PluginDeviceMutation::Set {
                        space: "string".into(),
                        key: "mine".into(),
                        value: "local".into(),
                    }],
                )
                .expect("write a local plugin value");
        }
        joined(&mut store, 5, 1);

        assert!(capture_state_sections(
            &mut store,
            &Sequence::from(13u64),
            &parent_sections(12, 11),
            "connection",
            "library",
            &spool.path().join("published"),
            &Cancellation::default(),
        )
        .is_err());
        let device = store.device_store_mut().expect("open device store");
        assert!(!device
            .read_section_cursor("connection", "library", Section::LocalPlugins)
            .expect("read the cursor")
            .expect("the cursor row stays")
            .joined());
        assert!(device
            .sections_await_publication("connection", "library")
            .expect("read awaiting publication"));
    }

    fn now_ms() -> u64 {
        u64::try_from(crate::persistent_store::device_store::now_ms().expect("read the device clock"))
            .expect("a time after the epoch")
    }

    /// A removal past the retention period stops being carried, and the floor
    /// moves up to the commit it was first published in. Neither happens until
    /// the publication is confirmed, and a removal still inside the period or
    /// published above what the remote carries is left alone.
    #[test]
    fn a_capture_preserves_the_remote_floor_without_borrowing_another_lineages_floor() {
        for local_floor in [0u64, 90] {
            let root = tempfile::tempdir().unwrap();
            let spool = tempfile::tempdir().unwrap();
            let mut store = participating_plugin_store(root.path());
            let device = store.device_store_mut().unwrap();
            device.note_section_published(Section::LocalPlugins, &[], &[],
                &TombstonePublication { generation: Sequence::from(90u64), at_ms: now_ms() },
                &BTreeMap::new(), &Sequence::from(local_floor), None).unwrap();
            device.write_section_cursor("connection", "library", Section::LocalPlugins, &SectionCursor {
                applied_generation: Sequence::from(10u64), applied_gc_floor: Sequence::from(7u64),
                observed_max_write_clock: Sequence::from(0u64),
            }).unwrap();
            let (captured, publications) = capture_state_sections(&mut store, &Sequence::from(11u64),
                &parent_sections(10, 7), "connection", "library", spool.path(), &Cancellation::default()).unwrap();
            assert_eq!(captured.iter().find(|source| source.kind == SectionKind::LocalPlugins).unwrap().gc_floor, Sequence::from(7u64));
            let publication = publications.iter()
                .find(|publication| publication.section == Section::LocalPlugins).unwrap();
            let (_, metadata) = open_publication_index(&publication.publication_index_path).unwrap();
            assert_eq!(metadata.gc_floor, Sequence::from(7u64));
        }
    }

    #[test]
    fn e1_reclamation_ack_preserves_rewritten_rows() {
        for kind in [SectionKind::Hypa, SectionKind::LocalPlugins] {
            for replacement in 0..4 {
                let root = tempfile::tempdir().unwrap();
                let spool = tempfile::tempdir().unwrap();
                let mut store = PersistentStore::open(root.path()).unwrap();
                let section = section_of(kind).unwrap();
                let value = match kind {
                    SectionKind::Hypa => hypa_row(&"a".repeat(64), 10, "writer-a", 4),
                    _ => plugin_row("key", "new value", 10, "writer-a"),
                };
                let expired = now_ms() - TOMBSTONE_RETENTION_MS - 1;
                let old = SectionRow {
                    value: SectionValueRow::Tombstone {
                        first_published: Some(TombstonePublication {
                            generation: Sequence::from(5u64),
                            at_ms: expired,
                        }),
                    },
                    ..value.clone()
                };
                let device = store.device_store_mut().unwrap();
                for chosen in [Section::Hypa, Section::LocalPlugins] {
                    device.set_section_participating(chosen, chosen == Section::Hypa || chosen == section).unwrap();
                }
                device.apply_section_rows(section, &[old.clone()]).unwrap();
                device.write_section_cursor("connection", "library", section, &SectionCursor {
                    applied_generation: Sequence::from(6u64),
                    applied_gc_floor: Sequence::from(0u64),
                    observed_max_write_clock: Sequence::from(10u64),
                }).unwrap();
                let mut reference = section_reference(6, 0);
                reference.kind = kind;
                let (captured, publications) = capture_state_sections(
                    &mut store, &Sequence::from(7u64),
                    &BTreeMap::from([(kind.id().to_owned(), reference)]),
                    "connection", "library", spool.path(), &Cancellation::default(),
                ).unwrap();
                let publication = publications.iter().find(|publication| publication.section == section).unwrap();
                assert_eq!(publication_evidence_count(publication, 1), 1);
                let newer = match replacement {
                    0 => SectionRow { write_clock: Sequence::from(11u64), ..value.clone() },
                    1 => SectionRow { write_clock: Sequence::from(11u64), ..old.clone() },
                    2 => SectionRow { writer_id: "writer-b".into(), ..old.clone() },
                    _ => SectionRow {
                        value: SectionValueRow::Tombstone {
                            first_published: Some(TombstonePublication {
                                generation: Sequence::from(4u64), at_ms: expired,
                            }),
                        },
                        ..old.clone()
                    },
                };
                store.device_store_mut().unwrap().apply_section_rows(section, &[newer.clone()]).unwrap();
                let table = if section == Section::Hypa { "hypa_embeddings" } else { "plugin_device_storage" };
                store.device_store_mut().unwrap().connection()
                    .execute(&format!("UPDATE {table} SET published_clock=NULL"), []).unwrap();
                confirm_publication(&mut store, publication, captured_for(&captured, publication));
                let device = store.device_store_mut().unwrap();
                assert_eq!(device.read_section_rows(section).unwrap(), vec![newer], "{kind:?}, replacement {replacement}");
                assert!(device.sections_await_publication("connection", "library").unwrap());
                assert_eq!(device.section_state(section).unwrap().gc_floor, Sequence::from(5u64));
            }
        }
    }

    #[test]
    fn e1_publication_ack_does_not_stamp_or_publish_another_writer() {
        for kind in [SectionKind::Hypa, SectionKind::LocalPlugins] {
            let root = tempfile::tempdir().unwrap();
            let spool = tempfile::tempdir().unwrap();
            let mut store = PersistentStore::open(root.path()).unwrap();
            let section = section_of(kind).unwrap();
            let mut row = match kind {
                SectionKind::Hypa => hypa_row(&"a".repeat(64), 10, "writer-a", 4),
                _ => plugin_row("key", "value", 10, "writer-a"),
            };
            row.value = SectionValueRow::Tombstone { first_published: None };
            let device = store.device_store_mut().unwrap();
            for chosen in [Section::Hypa, Section::LocalPlugins] {
                device.set_section_participating(chosen, chosen == Section::Hypa || chosen == section).unwrap();
            }
            device.apply_section_rows(section, &[row.clone()]).unwrap();
            let (captured, publications) = capture_state_sections(
                &mut store, &Sequence::from(7u64), &BTreeMap::new(),
                "connection", "library", spool.path(), &Cancellation::default(),
            ).unwrap();
            row.writer_id = "writer-b".into();
            store.device_store_mut().unwrap().apply_section_rows(section, &[row.clone()]).unwrap();
            let table = if section == Section::Hypa { "hypa_embeddings" } else { "plugin_device_storage" };
            store.device_store_mut().unwrap().connection()
                .execute(&format!("UPDATE {table} SET published_clock=NULL"), []).unwrap();
            let publication = publications.iter().find(|publication| publication.section == section).unwrap();
            confirm_publication(&mut store, publication, captured_for(&captured, publication));
            let device = store.device_store_mut().unwrap();
            assert_eq!(device.read_section_rows(section).unwrap(), vec![row]);
            let pending: bool = device.connection().query_row(
                &format!("SELECT published_clock IS NULL FROM {table}"), [], |row| row.get(0),
            ).unwrap();
            assert!(pending);
        }
    }

    #[test]
    fn confirmed_publication_omits_expired_payload_marker_but_preserves_local_evidence() {
        let spool = tempfile::tempdir().expect("create spool");
        let root = tempfile::tempdir().expect("create store root");
        let mut store = participating_plugin_store(root.path());
        let now = now_ms();
        let expired = now - TOMBSTONE_RETENTION_MS - 1;
        store
            .device_store_mut()
            .expect("open device store")
            .apply_section_rows(
                Section::LocalPlugins,
                &[
                    plugin_tombstone("old", 10, "writer-a", Some((5, expired))),
                    plugin_tombstone("recent", 11, "writer-a", Some((5, now))),
                    plugin_tombstone("unreached", 12, "writer-a", Some((9, expired))),
                    plugin_row("kept", "from-a", 13, "writer-a"),
                ],
            )
            .expect("take the remote rows");
        joined(&mut store, 6, 13);

        let (captured, publications) = capture_state_sections(
            &mut store,
            &Sequence::from(7u64),
            &parent_sections(6, 0),
            "connection",
            "library",
            &spool.path().join("published"),
            &Cancellation::default(),
        )
        .expect("capture the state sections");
        let publication = publications
            .iter()
            .find(|publication| publication.section == Section::LocalPlugins)
            .expect("a plugin publication");
        assert_eq!(publication_evidence_count(publication, 1), 1);
        let (_, publication_meta) = open_publication_index(&publication.publication_index_path)
            .expect("open publication index");
        assert_eq!(publication_meta.gc_floor, Sequence::from(5u64));
        let plugins = captured
            .iter()
            .find(|section| section.kind == SectionKind::LocalPlugins)
            .expect("a published plugin section");
        assert_eq!(plugins.gc_floor, Sequence::from(5u64));
        assert_eq!(
            published_plugin_values(plugins)
                .into_iter()
                .map(|(key, _)| key)
                .collect::<Vec<_>>(),
            vec![
                "kept".to_owned(),
                "recent".to_owned(),
                "unreached".to_owned()
            ]
        );

        // A publication that never finished reclaims nothing.
        assert!(held_plugin_keys(&mut store).contains(&("old".to_owned(), true)));
        assert_eq!(
            store
                .device_store_mut()
                .expect("open device store")
                .section_state(Section::LocalPlugins)
                .expect("read section state")
                .gc_floor,
            Sequence::from(0u64)
        );

        for publication in &publications {
            confirm_publication(&mut store, publication, captured_for(&captured, publication));
        }
        assert_eq!(
            held_plugin_keys(&mut store),
            vec![
                ("kept".to_owned(), false),
                ("old".to_owned(), true),
                ("recent".to_owned(), true),
                ("unreached".to_owned(), true),
            ]
        );
        assert_retained_removal(&mut store, plugin_tombstone("old", 10, "writer-a", Some((5, expired))));
        assert_eq!(
            store
                .device_store_mut()
                .expect("open device store")
                .section_state(Section::LocalPlugins)
                .expect("read section state")
                .gc_floor,
            Sequence::from(5u64)
        );
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
