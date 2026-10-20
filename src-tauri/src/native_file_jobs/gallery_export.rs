use super::error::{
    cancelled, destination_error_with, finish_with_release, invalid_input, io_error, job_error,
    store_error,
};
use super::{JobControl, JobPhase, JobProgress, JobResultSummary, NativeJobError};
use crate::asset_repository::{
    job_pins::{CasJobKind, CasJobOwner, CasObjectRole, CasReleaseOutcome, DurableCasJob},
    PayloadCas,
};
use crate::persistent_store::{
    export::destination, gallery::pinned_gallery_alias, PreparedRisuSaveExport,
};
use crate::server_sync::residency::HydrationSession;
use rusqlite::{params, OptionalExtension};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;
use uuid::Uuid;

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub(crate) enum GallerySelection {
    Asset { entries: Vec<GalleryEntry> },
    Inlay { keys: Vec<String> },
}
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct GalleryEntry {
    key: String,
    name: String,
    section: String,
}
impl GallerySelection {
    pub(crate) fn validate(&self, archive: bool) -> Result<(), NativeJobError> {
        let count = match self {
            Self::Asset { entries } => entries.len(),
            Self::Inlay { keys } => keys.len(),
        };
        if count == 0 || (!archive && count != 1) {
            return Err(invalid_input("gallery download selection is invalid"));
        }
        Ok(())
    }
    pub(crate) fn single_alias(&self) -> Result<(&'static str, &str), NativeJobError> {
        self.validate(false)?;
        Ok(match self {
            Self::Asset { entries } => ("asset", entries[0].key.as_str()),
            Self::Inlay { keys } => ("inlay", keys[0].as_str()),
        })
    }
    fn into_entries(
        self,
    ) -> (
        &'static str,
        Box<dyn ExactSizeIterator<Item = GalleryEntry>>,
    ) {
        match self {
            Self::Asset { entries } => ("asset", Box::new(entries.into_iter())),
            Self::Inlay { keys } => (
                "inlay",
                Box::new(keys.into_iter().map(|key| GalleryEntry {
                    key,
                    name: String::new(),
                    section: String::new(),
                })),
            ),
        }
    }
}

const MAX_COMPONENT_BYTES: usize = 255;

fn utf8_prefix(value: &str, max_bytes: usize) -> &str {
    let mut end = value.len().min(max_bytes);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

fn clean_component(value: &str, fallback: &str, max_bytes: usize) -> String {
    let cleaned: String = value
        .chars()
        .map(|c| {
            if c.is_control() || "\\/:*?\"<>|".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim_end_matches(['.', ' ']);
    let cleaned: String = cleaned.chars().take(120).collect();
    let cleaned = cleaned.trim_end_matches(['.', ' ']);
    let cleaned = if cleaned.is_empty() { fallback } else { cleaned };
    let base = cleaned.split('.').next().unwrap_or("").trim_end_matches(' ').to_ascii_uppercase();
    let reserved = matches!(base.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
        || ["COM", "LPT"].iter().any(|prefix| {
            base.strip_prefix(*prefix).is_some_and(|number| {
                matches!(number, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³")
            })
        });
    let cleaned = if reserved {
        format!("_{cleaned}")
    } else {
        cleaned.to_owned()
    };
    utf8_prefix(&cleaned, max_bytes).trim_end_matches(['.', ' ']).to_owned()
}
fn filename(name: &str, key: &str, ext: &str) -> String {
    let name = if name.trim().is_empty() || name == key {
        key
    } else {
        name
    };
    let stem = match name.rsplit_once('.') {
        Some((stem, suffix))
            if suffix.eq_ignore_ascii_case(ext.trim_start_matches('.'))
                || matches!(
                    suffix.to_ascii_lowercase().as_str(),
                    "jpg"
                        | "jpeg"
                        | "png"
                        | "gif"
                        | "webp"
                        | "avif"
                        | "wav"
                        | "mp3"
                        | "ogg"
                        | "flac"
                        | "webm"
                        | "mp4"
                        | "mkv"
                ) =>
        {
            stem
        }
        _ => name,
    };
    let ext = clean_component(ext.trim_start_matches('.'), "bin", 120);
    format!("{}.{}", clean_component(stem, "file", MAX_COMPONENT_BYTES - ext.len() - 1), ext)
}
struct Names {
    connection: rusqlite::Connection,
}
impl Names {
    fn new(path: &Path) -> Result<Self, NativeJobError> {
        let connection =
            crate::sqlite_open::open(path).map_err(|error| store_error(error.into()))?;
        // Disposable job state: spill names to disk with a fixed cache, without a durable journal.
        connection.execute_batch("PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF; PRAGMA cache_size=-512; PRAGMA mmap_size=0;
            CREATE TABLE used(path TEXT PRIMARY KEY) WITHOUT ROWID;
            CREATE TABLE suffixes(name TEXT PRIMARY KEY, next INTEGER NOT NULL) WITHOUT ROWID;")
            .map_err(|error| store_error(error.into()))?;
        Ok(Self { connection })
    }

    fn trim_cache(&self) -> rusqlite::Result<()> {
        let mut current = 0;
        let mut highwater = 0;
        let code = unsafe {
            rusqlite::ffi::sqlite3_db_status(
                self.connection.handle(),
                rusqlite::ffi::SQLITE_DBSTATUS_CACHE_USED,
                &mut current,
                &mut highwater,
                0,
            )
        };
        if code != rusqlite::ffi::SQLITE_OK {
            return Err(rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(code), None));
        }
        // The bundled SQLite global LRU can lend this cache pages beyond cache_size.
        if current > 512 * 1024 {
            self.connection.release_memory()?;
        }
        Ok(())
    }
}
fn unique_path(folder: &str, name: &str, names: &mut Names) -> Result<String, NativeJobError> {
    let (stem, ext) = name.rsplit_once('.').unwrap_or((name, "bin"));
    let identity = format!("{folder}/{name}").to_lowercase();
    let result = (|| -> rusqlite::Result<String> {
        let mut number = names
            .connection
            .prepare_cached("SELECT next FROM suffixes WHERE name=?1")?
            .query_row([&identity], |row| row.get::<_, i64>(0))
            .optional()?
            .unwrap_or(1);
        loop {
            let basename = if number == 1 {
                name.to_owned()
            } else {
                let suffix = format!(" ({number})");
                let stem = utf8_prefix(stem, MAX_COMPONENT_BYTES - ext.len() - suffix.len() - 1)
                    .trim_end_matches(['.', ' ']);
                format!("{stem}{suffix}.{ext}")
            };
            let path = if folder.is_empty() {
                basename
            } else {
                format!("{folder}/{basename}")
            };
            if names
                .connection
                .prepare_cached("INSERT OR IGNORE INTO used(path) VALUES(?1)")?
                .execute([path.to_lowercase()])?
                != 0
            {
                names.connection.prepare_cached("INSERT INTO suffixes(name,next) VALUES(?1,?2) ON CONFLICT(name) DO UPDATE SET next=excluded.next")?
                    .execute(params![identity, number + 1])?;
                names.trim_cache()?;
                return Ok(path);
            }
            number += 1;
        }
    })();
    result.map_err(|error| store_error(error.into()))
}

fn check(job: &JobControl) -> Result<(), NativeJobError> {
    if job.is_cancel_requested() {
        Err(cancelled("gallery download cancelled"))
    } else {
        Ok(())
    }
}
fn copy(
    source: &mut impl Read,
    output: &mut impl Write,
    job: &JobControl,
) -> Result<(u64, String, u32), NativeJobError> {
    let mut buffer = [0u8; 64 * 1024];
    let mut hash = Sha256::new();
    let mut crc = crc32fast::Hasher::new();
    let mut size = 0;
    loop {
        check(job)?;
        let count = source.read(&mut buffer).map_err(|_| {
            NativeJobError::new("unreadable-item", "gallery source could not be read")
        })?;
        if count == 0 {
            break;
        }
        output.write_all(&buffer[..count]).map_err(io_error)?;
        hash.update(&buffer[..count]);
        crc.update(&buffer[..count]);
        size += count as u64;
    }
    Ok((size, hex::encode(hash.finalize()), crc.finalize()))
}

// Store the central directory on disk, so its memory does not grow with entry count.
struct StoredZip<W: Write + Seek = File> {
    output: BufWriter<W>,
    central: BufWriter<File>,
    entries: u64,
}
fn u16le(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_le_bytes());
}
fn u32le(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}
fn u64le(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_le_bytes());
}
impl<W: Write + Seek> StoredZip<W> {
    fn new(output: W, central: &Path) -> Result<Self, NativeJobError> {
        let central = fs::OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(central)
            .map_err(io_error)?;
        Ok(Self {
            output: BufWriter::new(output),
            central: BufWriter::new(central),
            entries: 0,
        })
    }
    fn append(
        &mut self,
        name: &str,
        size: u64,
        crc: u32,
        source: &mut impl Read,
        job: &JobControl,
    ) -> Result<(), NativeJobError> {
        let name_len = u16::try_from(name.len())
            .map_err(|_| invalid_input("gallery archive name is too long"))?;
        let offset = self.output.stream_position().map_err(io_error)?;
        let mut header = Vec::with_capacity(64 + name.len());
        u32le(&mut header, 0x04034b50);
        for value in [45, 0x0800, 0, 0, 0x0021] {
            u16le(&mut header, value);
        }
        u32le(&mut header, crc);
        u32le(&mut header, u32::MAX);
        u32le(&mut header, u32::MAX);
        u16le(&mut header, name_len);
        u16le(&mut header, 20);
        header.extend_from_slice(name.as_bytes());
        u16le(&mut header, 1);
        u16le(&mut header, 16);
        u64le(&mut header, size);
        u64le(&mut header, size);
        self.output.write_all(&header).map_err(io_error)?;
        let (copied, _, copied_crc) = copy(source, &mut self.output, job)?;
        if copied != size || copied_crc != crc {
            return Err(NativeJobError::new(
                "unreadable-item",
                "staged gallery member changed",
            ));
        }
        header.clear();
        u32le(&mut header, 0x02014b50);
        for value in [45, 45, 0x0800, 0, 0, 0x0021] {
            u16le(&mut header, value);
        }
        u32le(&mut header, crc);
        u32le(&mut header, u32::MAX);
        u32le(&mut header, u32::MAX);
        for value in [name_len, 28, 0, 0, 0] {
            u16le(&mut header, value);
        }
        u32le(&mut header, 0);
        u32le(&mut header, u32::MAX);
        header.extend_from_slice(name.as_bytes());
        u16le(&mut header, 1);
        u16le(&mut header, 24);
        u64le(&mut header, size);
        u64le(&mut header, size);
        u64le(&mut header, offset);
        self.central.write_all(&header).map_err(io_error)?;
        self.entries += 1;
        Ok(())
    }
    fn finish(mut self, job: &JobControl) -> Result<W, NativeJobError> {
        let central_offset = self.output.stream_position().map_err(io_error)?;
        self.central.flush().map_err(io_error)?;
        let central_size = self.central.stream_position().map_err(io_error)?;
        self.central.seek(SeekFrom::Start(0)).map_err(io_error)?;
        copy(self.central.get_mut(), &mut self.output, job)?;
        let zip64_offset = self.output.stream_position().map_err(io_error)?;
        let mut end = Vec::with_capacity(98);
        u32le(&mut end, 0x06064b50);
        u64le(&mut end, 44);
        u16le(&mut end, 45);
        u16le(&mut end, 45);
        u32le(&mut end, 0);
        u32le(&mut end, 0);
        u64le(&mut end, self.entries);
        u64le(&mut end, self.entries);
        u64le(&mut end, central_size);
        u64le(&mut end, central_offset);
        u32le(&mut end, 0x07064b50);
        u32le(&mut end, 0);
        u64le(&mut end, zip64_offset);
        u32le(&mut end, 1);
        u32le(&mut end, 0x06054b50);
        u16le(&mut end, 0);
        u16le(&mut end, 0);
        let count = self.entries.min(u16::MAX as u64) as u16;
        u16le(&mut end, count);
        u16le(&mut end, count);
        u32le(&mut end, central_size.min(u32::MAX as u64) as u32);
        u32le(&mut end, central_offset.min(u32::MAX as u64) as u32);
        u16le(&mut end, 0);
        self.output.write_all(&end).map_err(io_error)?;
        self.output.flush().map_err(io_error)?;
        self.output
            .into_inner()
            .map_err(|error| io_error(error.into_error()))
    }
}

pub(crate) fn export_gallery(
    mut prepared: PreparedRisuSaveExport,
    selection: GallerySelection,
    archive: bool,
    owned: &Path,
    handoffs: &Path,
    destination_path: Option<&Path>,
    job: &JobControl,
) -> Result<JobResultSummary, NativeJobError> {
    selection.validate(archive)?;
    let repository_root = prepared
        .repository_root()
        .map_err(store_error)?
        .to_path_buf();
    let cas = PayloadCas::new(&repository_root).map_err(io_error)?;
    let mut pins = DurableCasJob::begin(
        &repository_root,
        &job.id(),
        CasJobKind::OfficialPublicationOrExportPreparation,
        CasJobOwner::native_file_job(&job.id()),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64,
    )
    .map_err(io_error)?;
    let mut released = false;
    let outcome = (|| {
        job.start(JobPhase::WritingExport).map_err(job_error)?;
        check(job)?;
        let (kind, entries) = selection.into_entries();
        let total = entries.len() as u64;
        let mut hydration = HydrationSession::new(&repository_root, Some(job.cancellation_flag()))
            .map_err(|_| {
                NativeJobError::new("unreadable-item", "gallery payload access is unavailable")
            })?;
        let source_path = owned.join("gallery-output.bin");
        let mut output = File::create(&source_path).map_err(io_error)?;
        let mut zip = if archive {
            Some(StoredZip::new(
                output.try_clone().map_err(io_error)?,
                &owned.join("gallery-central.bin"),
            )?)
        } else {
            None
        };
        let mut used = Names::new(&owned.join("gallery-names.sqlite"))?;
        let mut skipped = 0u64;
        let mut bytes = 0u64;
        job.set_progress(JobProgress {
            completed_bytes: 0,
            total_bytes: None,
            completed_items: 0,
            total_items: Some(total),
        })
        .map_err(job_error)?;
        for (index, entry) in entries.into_iter().enumerate() {
            check(job)?;
            let reader = prepared.reader().map_err(store_error)?;
            let alias = pinned_gallery_alias(&reader.connection, &reader.target, kind, &entry.key)
                .map_err(store_error)?;
            let stage_path = owned.join("gallery-item.bin");
            let staged = (|| {
                let alias = alias.ok_or_else(|| {
                    NativeJobError::new("unreadable-item", "gallery alias is unavailable")
                })?;
                let hash = alias.object_hash.as_deref().ok_or_else(|| {
                    NativeJobError::new("unreadable-item", "gallery payload is unavailable")
                })?;
                let mut source = hydration
                    .open(hash, &|| {
                        if job.is_cancel_requested() {
                            return Err(crate::server_sync::SyncError::new("cancelled", 499));
                        }
                        Ok(())
                    })
                    .map_err(|error| {
                        if error.code == "local-storage-full" {
                            NativeJobError::new(
                                "storage-full",
                                "gallery payload could not be stored",
                            )
                        } else {
                            NativeJobError::new(
                                "unreadable-item",
                                "gallery payload could not be downloaded",
                            )
                        }
                    })?
                    .ok_or_else(|| {
                        NativeJobError::new("unreadable-item", "gallery payload is unavailable")
                    })?;
                let expected = u64::try_from(alias.size)
                    .map_err(|_| invalid_input("gallery payload size is invalid"))?;
                pins.pin_existing(&cas, hash, expected, CasObjectRole::DirectObject)
                    .map_err(|error| {
                        if error.kind() == std::io::ErrorKind::InvalidData {
                            NativeJobError::new(
                                "unreadable-item",
                                "gallery payload could not be pinned",
                            )
                        } else {
                            io_error(error)
                        }
                    })?;
                let mut stage = File::create(&stage_path).map_err(io_error)?;
                let (size, actual_hash, crc) = copy(&mut source, &mut stage, job)?;
                if size != expected || actual_hash != hash {
                    return Err(NativeJobError::new(
                        "unreadable-item",
                        "gallery payload identity differs",
                    ));
                }
                stage.seek(SeekFrom::Start(0)).map_err(io_error)?;
                // Reopen read-only: File::create opens the staging file for writes only.
                drop(stage);
                let name = filename(
                    if kind == "inlay" {
                        &alias.name
                    } else {
                        &entry.name
                    },
                    &entry.key,
                    &alias.ext,
                );
                let folder = if archive && kind == "asset" {
                    clean_component(&entry.section, "assets", MAX_COMPONENT_BYTES)
                } else {
                    String::new()
                };
                Ok((
                    File::open(&stage_path).map_err(io_error)?,
                    unique_path(&folder, &name, &mut used)?,
                    size,
                    crc,
                ))
            })();
            check(job)?;
            match staged {
                Ok((mut stage, name, size, crc)) => {
                    if let Some(zip) = zip.as_mut() {
                        zip.append(&name, size, crc, &mut stage, job)?;
                    } else {
                        copy(&mut stage, &mut output, job)?;
                    }
                    bytes += size;
                }
                Err(error) if archive && error.code == "unreadable-item" => skipped += 1,
                Err(error) => return Err(error),
            }
            if stage_path.exists() {
                fs::remove_file(&stage_path).map_err(io_error)?;
            }
            job.set_progress(JobProgress {
                completed_bytes: bytes,
                total_bytes: None,
                completed_items: index as u64 + 1,
                total_items: Some(total),
            })
            .map_err(job_error)?;
        }
        if let Some(zip) = zip {
            zip.finish(job)?;
        }
        output.flush().map_err(io_error)?;
        output.sync_all().map_err(io_error)?;
        drop(output);
        check(job)?;
        prepared.release_reader().map_err(store_error)?;
        released = true;
        pins.release(CasReleaseOutcome::Aborted).map_err(io_error)?;
        job.set_phase(JobPhase::PublishingDestination)
            .map_err(job_error)?;
        let (root, target, handoff_path) = if let Some(path) = destination_path {
            (
                path.parent()
                    .ok_or_else(|| invalid_input("gallery destination has no parent"))?,
                path.to_owned(),
                None,
            )
        } else {
            fs::create_dir_all(handoffs).map_err(io_error)?;
            let path = handoffs.join(format!("risu-download-{}.bin", Uuid::new_v4()));
            (
                handoffs,
                path.clone(),
                Some(path.to_string_lossy().into_owned()),
            )
        };
        let published = destination::write_gallery_destination_controlled(
            owned,
            &source_path,
            root,
            &target,
            || job.is_cancel_requested(),
            |_| {},
            || {
                job.set_phase(JobPhase::FinalizingExport)
                    .map_err(|_| destination::DestinationWriteError::Cancelled)
            },
        )
        .map_err(|error| {
            destination_error_with(
                error,
                "gallery source is unavailable",
                "gallery destination is invalid",
                "gallery download cancelled",
            )
        })?;
        Ok(JobResultSummary {
            export_exclusions: None,
            revision: prepared.revision,
            source_bytes: published.bytes,
            source_sha256: published.sha256,
            source_fingerprint_kind: super::SourceFingerprintKind::WholeFileSha256,
            character_count: 0,
            preset_count: 0,
            warning_codes: if skipped == 0 {
                vec![]
            } else {
                vec![format!("gallery-skipped:{skipped}")]
            },
            handoff_path,
            publication: None,
        })
    })();
    let release_pins = pins.release(CasReleaseOutcome::Aborted).map_err(io_error);
    let outcome = if !released {
        finish_with_release(
            outcome,
            prepared.release_reader(),
            "gallery revision release failed",
        )
    } else {
        outcome
    };
    match (outcome, release_pins) {
        (Ok(result), Ok(())) => Ok(result),
        (Err(error), _) | (_, Err(error)) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn names_replace_media_suffixes_and_keep_distinct_occurrences() {
        assert_eq!(filename("photo.PNG", "id", "webp"), "photo.webp");
        assert_eq!(filename("gemini-audio", "id", "wav"), "gemini-audio.wav");
        assert_eq!(filename("theme.CSS", "id", "css"), "theme.css");
        assert_eq!(filename("", "id", "mp4"), "id.mp4");
        assert_eq!(filename("../bad:name. ", "id", "png"), ".._bad_name.png");
        for (name, ext, expected) in [
            ("한".repeat(120), "png".to_owned(), format!("{}.png", "한".repeat(83))),
            ("😀".repeat(120), "png".to_owned(), format!("{}.png", "😀".repeat(62))),
            ("한".repeat(120), "한".repeat(120), format!("{}.{}", "한".repeat(44), "한".repeat(40))),
            ("CON".to_owned(), "png".to_owned(), "_CON.png".to_owned()),
            ("aux.other".to_owned(), "png".to_owned(), "_aux.other.png".to_owned()),
            ("LPT¹".to_owned(), "png".to_owned(), "_LPT¹.png".to_owned()),
            ("COM10".to_owned(), "png".to_owned(), "COM10.png".to_owned()),
        ] {
            let actual = filename(&name, "id", &ext);
            assert_eq!(actual, expected);
            assert!(actual.len() <= MAX_COMPONENT_BYTES);
        }
        let directory = tempfile::TempDir::new().unwrap();
        let mut used = Names::new(&directory.path().join("names.sqlite")).unwrap();
        assert_eq!(
            unique_path("감정 이미지", "Smile.png", &mut used).unwrap(),
            "감정 이미지/Smile.png"
        );
        assert_eq!(
            unique_path("감정 이미지", "smile.png", &mut used).unwrap(),
            "감정 이미지/smile (2).png"
        );
        assert_eq!(
            unique_path("추가 에셋", "smile.png", &mut used).unwrap(),
            "추가 에셋/smile.png"
        );
    }
    fn fixture() -> (
        tempfile::TempDir,
        crate::persistent_store::PersistentStore,
        i64,
        crate::persistent_store::AssetAlias,
    ) {
        use serde_json::json;
        let directory = tempfile::TempDir::new().unwrap();
        let mut store = crate::persistent_store::PersistentStore::open(directory.path()).unwrap();
        let cas = PayloadCas::new(directory.path()).unwrap();
        let payload = cas.prepare_bytes(b"original synthetic image").unwrap();
        let alias = crate::persistent_store::AssetAlias {
            key: "inlay-1".into(),
            kind: "inlay".into(),
            object_hash: Some(payload.content_hash),
            size: payload.byte_size as i64,
            name: "photo.PNG".into(),
            ext: "png".into(),
            mime: "image/png".into(),
            inlay_type: Some("image".into()),
            width: None,
            height: None,
            metadata: json!({}),
        };
        let staging = store.replace_begin().unwrap().staging_id;
        store.replace_put_root(&staging, &json!({})).unwrap();
        store.replace_put_presets(&staging, &[]).unwrap();
        store
            .replace_put_asset_aliases(&staging, &[alias.clone()])
            .unwrap();
        let revision = store.replace_commit(&staging, Some(0)).unwrap().revision;
        (directory, store, revision, alias)
    }

    #[test]
    fn zip_keeps_leased_bytes_and_extension_after_alias_rewrite_and_counts_unreadable_items() {
        use crate::native_file_jobs::{JobKind, JobRegistry};
        let (directory, mut store, revision, mut alias) = fixture();
        let prepared = store.prepare_risu_save_export(revision).unwrap();
        let cas = PayloadCas::new(directory.path()).unwrap();
        let replacement = cas.prepare_bytes(b"new synthetic webp").unwrap();
        alias.object_hash = Some(replacement.content_hash);
        alias.size = replacement.byte_size as i64;
        alias.ext = "webp".into();
        store.commit_asset_alias(&alias, revision).unwrap();
        let owned = directory.path().join("owned");
        fs::create_dir(&owned).unwrap();
        let target = directory.path().join("chosen.zip");
        let job = JobRegistry::default()
            .create(JobKind::ExportGalleryZip)
            .unwrap();
        let result = export_gallery(
            prepared,
            GallerySelection::Inlay {
                keys: vec!["inlay-1".into(), "missing".into()],
            },
            true,
            &owned,
            &directory.path().join("handoffs"),
            Some(&target),
            &job,
        )
        .unwrap();
        assert_eq!(result.warning_codes, vec!["gallery-skipped:1"]);
        let mut archive = zip::ZipArchive::new(File::open(target).unwrap()).unwrap();
        assert_eq!(archive.len(), 1);
        let mut entry = archive.by_name("photo.png").unwrap();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"original synthetic image");
        assert_eq!(job.status().progress.completed_items, 2);
        assert!(fs::read_dir(directory.path().join("assets/job-pins"))
            .unwrap()
            .next()
            .is_none());
    }

    #[test]
    fn zip_names_extract_with_multibyte_suffixes_and_windows_reserved_names() {
        use crate::native_file_jobs::{JobKind, JobRegistry};
        let (directory, mut store, revision, mut alias) = fixture();
        alias.name = "한".repeat(120);
        let revision = store.commit_asset_alias(&alias, revision).unwrap().revision;
        alias.key = "inlay-2".into();
        alias.name = "CON".into();
        let revision = store.commit_asset_alias(&alias, revision).unwrap().revision;
        let owned = directory.path().join("owned");
        fs::create_dir(&owned).unwrap();
        let target = directory.path().join("chosen.zip");
        let job = JobRegistry::default().create(JobKind::ExportGalleryZip).unwrap();
        let mut keys = vec!["inlay-1".into(); 12];
        keys.push("inlay-2".into());
        export_gallery(
            store.prepare_risu_save_export(revision).unwrap(),
            GallerySelection::Inlay { keys },
            true,
            &owned,
            &directory.path().join("handoffs"),
            Some(&target),
            &job,
        ).unwrap();
        let mut archive = zip::ZipArchive::new(File::open(&target).unwrap()).unwrap();
        assert_eq!(archive.len(), 13);
        let mut expected = vec![format!("{}.png", "한".repeat(83))];
        expected.extend((2..=12).map(|number| format!("{} ({number}).png", "한".repeat(82))));
        expected.push("_CON.png".into());
        for (index, name) in expected.iter().enumerate() {
            let entry = archive.by_index(index).unwrap();
            assert_eq!(entry.name(), name);
            assert!(entry.name().len() <= MAX_COMPONENT_BYTES);
            let date = entry.last_modified();
            assert_eq!((date.year(), date.month(), date.day()), (1980, 1, 1));
        }
        let extracted = directory.path().join("extracted");
        archive.extract(&extracted).unwrap();
        for name in expected {
            assert_eq!(fs::read(extracted.join(name)).unwrap(), b"original synthetic image");
        }
        let bytes = fs::read(target).unwrap();
        assert_eq!(&bytes[12..14], &0x0021u16.to_le_bytes());
    }

    #[test]
    fn single_download_hydrates_remote_bytes_and_returns_an_owned_mobile_handoff() {
        use crate::native_file_jobs::{JobKind, JobRegistry};
        use crate::server_sync::residency::test_remote;
        let (directory, mut store, revision, alias) = fixture();
        let cas = PayloadCas::new(directory.path()).unwrap();
        let hash = alias.object_hash.as_deref().unwrap();
        let bytes = cas.read_object(hash).unwrap().unwrap();
        fs::remove_file(cas.object_path(hash).unwrap().unwrap()).unwrap();
        test_remote::hold(directory.path(), &[(hash, bytes.len() as u64)]);
        test_remote::serve(directory.path(), hash, bytes.clone());
        let owned = directory.path().join("owned");
        fs::create_dir(&owned).unwrap();
        let handoffs = directory.path().join("handoffs");
        let job = JobRegistry::default()
            .create(JobKind::ExportGalleryFile)
            .unwrap();
        let prepared = store.prepare_gallery_file_export(revision, "inlay", &alias.key).unwrap();
        let readers = prepared.reader().unwrap().active_readers();
        let result = export_gallery(
            prepared,
            GallerySelection::Inlay {
                keys: vec![alias.key],
            },
            false,
            &owned,
            &handoffs,
            None,
            &job,
        )
        .unwrap();
        assert_eq!(readers.active_count(), 0);
        assert!(readers.detached_asset_roots().unwrap().is_empty());
        assert_eq!(test_remote::fetched(directory.path()), 1);
        let handoff = std::path::PathBuf::from(result.handoff_path.unwrap());
        assert_eq!(handoff.parent(), Some(handoffs.as_path()));
        assert!(super::super::handoff_name(
            &handoff,
            "risu-download-",
            ".bin"
        ));
        assert_eq!(fs::read(handoff).unwrap(), bytes);
    }

    #[test]
    fn single_missing_file_and_cancel_preserve_the_chosen_destination() {
        use crate::native_file_jobs::{JobKind, JobRegistry};
        let (directory, mut store, revision, _) = fixture();
        let owned = directory.path().join("owned");
        fs::create_dir(&owned).unwrap();
        let target = directory.path().join("chosen.png");
        fs::write(&target, b"existing").unwrap();
        let job = JobRegistry::default()
            .create(JobKind::ExportGalleryFile)
            .unwrap();
        let prepared = store.prepare_gallery_file_export(revision, "inlay", "missing").unwrap();
        let readers = prepared.reader().unwrap().active_readers();
        let error = export_gallery(
            prepared,
            GallerySelection::Inlay {
                keys: vec!["missing".into()],
            },
            false,
            &owned,
            &directory.path().join("handoffs"),
            Some(&target),
            &job,
        )
        .unwrap_err();
        assert_eq!(readers.active_count(), 0);
        assert!(readers.detached_asset_roots().unwrap().is_empty());
        assert_eq!(error.code, "unreadable-item");
        assert_eq!(fs::read(&target).unwrap(), b"existing");
        let registry = JobRegistry::default();
        let job = registry.create(JobKind::ExportGalleryFile).unwrap();
        job.cancellation_flag()
            .store(true, std::sync::atomic::Ordering::Release);
        let prepared = store.prepare_gallery_file_export(revision, "inlay", "inlay-1").unwrap();
        let readers = prepared.reader().unwrap().active_readers();
        let error = export_gallery(
            prepared,
            GallerySelection::Inlay {
                keys: vec!["inlay-1".into()],
            },
            false,
            &owned,
            &directory.path().join("handoffs"),
            Some(&target),
            &job,
        )
        .unwrap_err();
        assert_eq!(readers.active_count(), 0);
        assert!(readers.detached_asset_roots().unwrap().is_empty());
        assert_eq!(error.code, "cancelled");
        assert_eq!(fs::read(&target).unwrap(), b"existing");
        assert!(!directory.path().join("handoffs").exists());
    }

    #[test]
    fn zip_writer_uses_zip64_beyond_the_entry_count_boundary() {
        use crate::native_file_jobs::{JobKind, JobRegistry};
        let directory = tempfile::TempDir::new().unwrap();
        let target = directory.path().join("output.zip");
        let mut writer = StoredZip::new(
            File::create(&target).unwrap(),
            &directory.path().join("central"),
        )
        .unwrap();
        let job = JobRegistry::default()
            .create(JobKind::ExportGalleryZip)
            .unwrap();
        for index in 0..65_536 {
            writer
                .append(&format!("{index}.png"), 0, 0, &mut std::io::empty(), &job)
                .unwrap();
        }
        writer.finish(&job).unwrap();
        let mut archive = zip::ZipArchive::new(File::open(target).unwrap()).unwrap();
        assert_eq!(archive.len(), 65_536);
        assert_eq!(archive.by_index(65_535).unwrap().name(), "65535.png");
    }
    #[test]
    fn generated_payload_copy_has_bounded_reads_and_stops_during_a_member() {
        use crate::native_file_jobs::{JobKind, JobRegistry};
        struct Generated {
            remaining: usize,
            largest: usize,
            cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
        }
        impl Read for Generated {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                self.largest = self.largest.max(buffer.len());
                assert!(buffer.len() <= 64 * 1024, "body reads must stay bounded");
                let count = self.remaining.min(buffer.len());
                buffer[..count].fill(42);
                self.remaining -= count;
                if count > 0 {
                    if let Some(cancel) = &self.cancel {
                        cancel.store(true, std::sync::atomic::Ordering::Release);
                    }
                }
                Ok(count)
            }
        }
        struct VerifiedSink {
            bytes: usize,
        }
        impl Write for VerifiedSink {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                assert!(bytes.iter().all(|byte| *byte == 42));
                self.bytes += bytes.len();
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let job = JobRegistry::default()
            .create(JobKind::ExportGalleryZip)
            .unwrap();
        let mut source = Generated {
            remaining: 8 * 1024 * 1024,
            largest: 0,
            cancel: None,
        };
        let mut sink = VerifiedSink { bytes: 0 };
        assert_eq!(
            copy(&mut source, &mut sink, &job).unwrap().0,
            8 * 1024 * 1024
        );
        assert_eq!(sink.bytes, 8 * 1024 * 1024);
        source.remaining = 8 * 1024 * 1024;
        source.cancel = Some(job.cancellation_flag());
        sink.bytes = 0;
        assert_eq!(
            copy(&mut source, &mut sink, &job).unwrap_err().code,
            "cancelled"
        );
        assert_eq!(sink.bytes, 64 * 1024);
        assert!(source.remaining > 0);
    }

    #[test]
    fn cancelled_running_gallery_job_removes_partial_files_before_its_terminal_receipt() {
        use crate::native_file_jobs::{JobState, NativeFileJobState, NativeFileJobTask};
        let (directory, mut store, revision, _) = fixture();
        let native_root = directory.path().join("native-file-jobs");
        let state = NativeFileJobState::initialize(native_root.clone());
        let target = directory.path().join("existing.zip");
        fs::write(&target, b"existing destination").unwrap();
        let started = state
            .spawn(
                NativeFileJobTask::ExportGallery {
                    destination: Some(target.clone()),
                    prepared: store.prepare_risu_save_export(revision).unwrap(),
                    selection: GallerySelection::Inlay {
                        keys: vec!["inlay-1".into(); 100_000],
                    },
                    archive: true,
                },
                false,
            )
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            let status = state.status(&started.job_id).unwrap();
            assert!(
                !status.state.is_terminal(),
                "job must still be writing when cancellation is requested"
            );
            if status.progress.completed_items > 0 {
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(native_root
            .join("jobs")
            .join(&started.job_id)
            .join("gallery-output.bin")
            .exists());
        state.cancel(&started.job_id).unwrap();
        loop {
            let status = state.status(&started.job_id).unwrap();
            if status.state.is_terminal() {
                assert_eq!(status.state, JobState::Cancelled);
                break;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert!(!native_root.join("jobs").join(&started.job_id).exists());
        assert!(fs::read_dir(native_root.join("handoffs"))
            .unwrap()
            .next()
            .is_none());
        assert_eq!(fs::read(target).unwrap(), b"existing destination");
    }

    #[test]
    fn filename_index_spills_large_selections_and_keeps_its_cache_bounded() {
        let directory = tempfile::TempDir::new().unwrap();
        // Other live stores share SQLite's global LRU pool in bundled builds.
        let other = crate::sqlite_open::open(directory.path().join("other.sqlite")).unwrap();
        other.execute_batch("PRAGMA journal_mode=OFF; PRAGMA synchronous=OFF; PRAGMA cache_size=-8192;
            CREATE TABLE payload(value BLOB);
            WITH RECURSIVE rows(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM rows WHERE n<1024)
            INSERT INTO payload SELECT zeroblob(4096) FROM rows;").unwrap();
        let mut names = Names::new(&directory.path().join("names.sqlite")).unwrap();
        for index in 0..20_000 {
            let name = format!("image-{}-{index}.png", "x".repeat(80));
            assert_eq!(unique_path("", &name, &mut names).unwrap(), name);
        }
        assert_eq!(
            unique_path("", &format!("IMAGE-{}-19999.png", "X".repeat(80)), &mut names).unwrap(),
            format!("IMAGE-{}-19999 (2).png", "X".repeat(80))
        );
        assert_eq!(
            unique_path("", &format!("IMAGE-{}-19999 (2).png", "X".repeat(80)), &mut names).unwrap(),
            format!("IMAGE-{}-19999 (2) (2).png", "X".repeat(80))
        );
        assert!(fs::metadata(directory.path().join("names.sqlite")).unwrap().len() > 2 * 1024 * 1024);
        let mut current = 0;
        let mut highwater = 0;
        // Inspect SQLite's actual retained page-cache memory, excluding the on-disk index size.
        let code = unsafe {
            rusqlite::ffi::sqlite3_db_status(
                names.connection.handle(),
                rusqlite::ffi::SQLITE_DBSTATUS_CACHE_USED,
                &mut current,
                &mut highwater,
                0,
            )
        };
        assert_eq!(code, rusqlite::ffi::SQLITE_OK);
        assert!(
            current < 1024 * 1024,
            "filename cache grew to {current} bytes"
        );
    }
    #[test]
    fn zip64_offsets_beyond_four_gib_are_readable_without_a_giant_payload_fixture() {
        use crate::native_file_jobs::{JobKind, JobRegistry};
        // Sparse filesystem boundary: retain only written extents, with a zero-filled logical hole.
        #[derive(Default)]
        struct SparseFile {
            position: u64,
            length: u64,
            extents: Vec<(u64, Vec<u8>)>,
        }
        impl Write for SparseFile {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.extents.push((self.position, bytes.to_vec()));
                self.position += bytes.len() as u64;
                self.length = self.length.max(self.position);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl Seek for SparseFile {
            fn seek(&mut self, from: SeekFrom) -> std::io::Result<u64> {
                let next = match from {
                    SeekFrom::Start(offset) => offset as i128,
                    SeekFrom::Current(offset) => self.position as i128 + offset as i128,
                    SeekFrom::End(offset) => self.length as i128 + offset as i128,
                };
                self.position = u64::try_from(next)
                    .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
                Ok(self.position)
            }
        }
        impl Read for SparseFile {
            fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
                let count =
                    (self.length.saturating_sub(self.position)).min(bytes.len() as u64) as usize;
                bytes[..count].fill(0);
                for (offset, extent) in &self.extents {
                    let start = self.position.max(*offset);
                    let end = (self.position + count as u64).min(*offset + extent.len() as u64);
                    if start < end {
                        bytes[(start - self.position) as usize..(end - self.position) as usize]
                            .copy_from_slice(
                                &extent[(start - *offset) as usize..(end - *offset) as usize],
                            );
                    }
                }
                self.position += count as u64;
                Ok(count)
            }
        }
        let directory = tempfile::TempDir::new().unwrap();
        let mut sparse = SparseFile::default();
        sparse.seek(SeekFrom::Start(u32::MAX as u64 + 128)).unwrap();
        let mut writer = StoredZip::new(sparse, &directory.path().join("central")).unwrap();
        let payload = b"synthetic media";
        let job = JobRegistry::default()
            .create(JobKind::ExportGalleryZip)
            .unwrap();
        writer
            .append(
                "photo.png",
                payload.len() as u64,
                crc32fast::hash(payload),
                &mut &payload[..],
                &job,
            )
            .unwrap();
        let sparse = writer.finish(&job).unwrap();
        assert!(
            sparse
                .extents
                .iter()
                .map(|(_, bytes)| bytes.len())
                .sum::<usize>()
                < 1024
        );
        let mut archive = zip::ZipArchive::new(sparse).unwrap();
        assert_eq!(archive.len(), 1);
        let mut body = Vec::new();
        archive
            .by_name("photo.png")
            .unwrap()
            .read_to_end(&mut body)
            .unwrap();
        assert_eq!(body, payload);
    }
}
