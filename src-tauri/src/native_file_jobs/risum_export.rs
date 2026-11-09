use super::error::{
    cancelled, destination_error_with, invalid_input, io_error, job_error, store_error,
};
use super::card_mapping::{MAPPING_LIMITS, MAPPING_STACK_BYTES, MAX_MAPPED_CARD_BYTES};
use super::{JobControl, JobPhase, JobProgress, JobResultSummary, NativeJobError};
use crate::asset_repository::owner_manifest_codec::OwnerManifestEntry;
use crate::asset_repository::PayloadCas;
use crate::persistent_store::{
    export::{self, destination},
    PreparedRisuSaveExport, RevisionReadLease, StoreResult,
};
use crate::server_sync::residency::RemotePayloadAccess;
use risunest_formats::module_export::risum_metadata_text;
use risunest_ir::json::parse_json;
use risunest_ir::Budget;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::fs::{self, OpenOptions};
use std::io::{BufWriter, Read, Write};
use std::path::Path;
use std::sync::{atomic::AtomicBool, Arc};
use uuid::Uuid;

const RPACK_MAP: &[u8; 512] = include_bytes!("../../../src/ts/rpack/rpack_map.bin");

#[cfg(test)]
fn write_risum(
    output: &mut impl Write,
    module: &Value,
    payloads: &[Vec<u8>],
    is_cancelled: impl Fn() -> bool,
) -> Result<(), NativeJobError> {
    let mut projected = module.clone();
    let object = projected
        .as_object_mut()
        .ok_or_else(|| invalid_input("pinned root module must be an object"))?;
    let assets_present = object.contains_key("assets");
    let assets = object
        .get_mut("assets")
        .map(|value| {
            value
                .as_array_mut()
                .ok_or_else(|| invalid_input("pinned root module assets must be an array"))
        })
        .transpose()?;
    let asset_count = assets.as_ref().map_or(0, |assets| assets.len());
    if asset_count != payloads.len() {
        return Err(invalid_input(
            "RISUM payload count does not match module assets",
        ));
    }
    if let Some(assets) = assets {
        for (position, asset) in assets.iter_mut().enumerate() {
            let tuple = asset.as_array_mut().ok_or_else(|| {
                invalid_input(format!(
                    "pinned root module asset {position} must be a tuple"
                ))
            })?;
            if tuple.len() < 3
                || !tuple[0].is_string()
                || !tuple[1].is_string()
                || !tuple[2].is_string()
            {
                return Err(invalid_input(format!(
                    "pinned root module asset {position} tuple is invalid"
                )));
            }
            tuple[1] = Value::String(String::new());
        }
    }
    if !assets_present && !payloads.is_empty() {
        return Err(invalid_input(
            "absent RISUM assets cannot have payload frames",
        ));
    }
    let metadata = serde_json::to_vec_pretty(&json!({
        "module": projected,
        "type": "risuModule"
    }))
    .map_err(|error| invalid_input(error.to_string()))?;
    let metadata_length = u32::try_from(metadata.len())
        .map_err(|_| NativeJobError::too_large("module", "RISUM metadata exceeds its frame limit"))?;
    output.write_all(&[111, 0]).map_err(io_error)?;
    output
        .write_all(&metadata_length.to_le_bytes())
        .map_err(io_error)?;
    write_rpack(output, &metadata, &is_cancelled)?;
    for payload in payloads {
        if is_cancelled() {
            return Err(cancelled("RISUM export cancelled while writing assets"));
        }
        let length = u32::try_from(payload.len())
            .map_err(|_| NativeJobError::too_large("module", "RISUM asset exceeds its frame limit"))?;
        output.write_all(&[1]).map_err(io_error)?;
        output.write_all(&length.to_le_bytes()).map_err(io_error)?;
        write_rpack(output, payload, &is_cancelled)?;
    }
    output.write_all(&[0]).map_err(io_error)
}

fn write_rpack(
    output: &mut impl Write,
    bytes: &[u8],
    is_cancelled: &impl Fn() -> bool,
) -> Result<(), NativeJobError> {
    for chunk in bytes.chunks(64 * 1024) {
        if is_cancelled() {
            return Err(cancelled("RISUM export cancelled while encoding RPack"));
        }
        let encoded = chunk
            .iter()
            .map(|byte| RPACK_MAP[*byte as usize])
            .collect::<Vec<_>>();
        output.write_all(&encoded).map_err(io_error)?;
    }
    Ok(())
}

pub(crate) fn export_risu_module(
    prepared: PreparedRisuSaveExport,
    module_index: u64,
    owned_directory: &Path,
    handoff_directory: &Path,
    destination_path: Option<&Path>,
    job: &JobControl,
) -> Result<JobResultSummary, NativeJobError> {
    export_risu_module_with_release(
        prepared,
        module_index,
        owned_directory,
        handoff_directory,
        destination_path,
        job,
        PreparedRisuSaveExport::release_reader,
    )
}

#[allow(clippy::too_many_arguments)]
fn export_risu_module_with_release<F>(
    mut prepared: PreparedRisuSaveExport,
    module_index: u64,
    owned_directory: &Path,
    handoff_directory: &Path,
    destination_path: Option<&Path>,
    job: &JobControl,
    release_reader: F,
) -> Result<JobResultSummary, NativeJobError>
where
    F: FnOnce(&mut PreparedRisuSaveExport) -> StoreResult<()>,
{
    let mut release_reader = Some(release_reader);
    let outcome = (|| {
        if job.is_cancel_requested() {
            return Err(cancelled("RISUM export cancelled before encoding"));
        }
        job.start(JobPhase::WritingExport).map_err(job_error)?;
        let repository =
            PayloadCas::new(prepared.repository_root().map_err(store_error)?).map_err(io_error)?;
        let source = owned_directory.join("module.risum");
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&source)
            .map_err(io_error)?;
        let mut output = BufWriter::new(super::export::ProgressWriter::new(file, job));
        let _total_items = {
            let reader = prepared.reader().map_err(store_error)?;
            let projected = export::projected_root_module(
                &reader.connection,
                &prepared.snapshots_dir,
                &reader.target,
                module_index,
            )
            .map_err(store_error)?;
            write_projected_risum(
                &mut output,
                projected.value,
                projected.asset_entries.as_deref(),
                reader,
                &repository,
                job,
            )?
        };
        output.flush().map_err(io_error)?;
        output.get_ref().sync_all().map_err(io_error)?;
        drop(output);
        let source_bytes = fs::metadata(&source).map_err(io_error)?.len();
        if job.is_cancel_requested() {
            return Err(cancelled(
                "RISUM export cancelled before destination publication",
            ));
        }
        release_reader.take().ok_or_else(|| {
            NativeJobError::new("store-error", "revision release was already used")
        })?(&mut prepared)
        .map_err(store_error)?;
        job.set_phase(JobPhase::PublishingDestination)
            .map_err(job_error)?;
        let (destination_root, destination_path, handoff_path) = match destination_path {
            Some(destination_path) => (
                destination_path.parent().ok_or_else(|| {
                    NativeJobError::new(
                        "invalid-destination",
                        "RISUM destination directory is unavailable",
                    )
                })?,
                destination_path.to_owned(),
                None,
            ),
            None => {
                fs::create_dir_all(handoff_directory).map_err(io_error)?;
                let path = handoff_directory.join(format!("risu-module-{}.risum", Uuid::new_v4()));
                (
                    handoff_directory,
                    path.clone(),
                    Some(path.to_string_lossy().into_owned()),
                )
            }
        };
        let phase_failure = RefCell::new(None);
        let published = destination::write_risu_module_destination_controlled(
            owned_directory,
            &source,
            destination_root,
            &destination_path,
            || job.is_cancel_requested() || phase_failure.borrow().is_some(),
            |progress| {
                if let Err(error) = job.set_export_bytes(progress.copied_bytes, Some(source_bytes)) {
                    *phase_failure.borrow_mut() = Some(error);
                }
            },
            || {
                if job.is_cancel_requested() || phase_failure.borrow().is_some() {
                    return Err(destination::DestinationWriteError::Cancelled);
                }
                job.set_phase(JobPhase::FinalizingExport).map_err(|error| {
                    *phase_failure.borrow_mut() = Some(error);
                    destination::DestinationWriteError::Cancelled
                })
            },
        )
        .map_err(|error| {
            phase_failure
                .into_inner()
                .map(job_error)
                .unwrap_or_else(|| destination_error(error))
        })?;
        Ok(JobResultSummary {
            export_exclusions: None,
            revision: prepared.revision,
            source_bytes: published.bytes,
            source_sha256: published.sha256,
            source_fingerprint_kind: crate::native_file_jobs::SourceFingerprintKind::WholeFileSha256,
            character_count: 0,
            preset_count: 0,
            warning_codes: Vec::new(),
            handoff_path,
            publication: None,
        })
    })();
    match release_reader.take() {
        Some(release_reader) => super::error::finish_with_release(
            outcome,
            release_reader(&mut prepared),
            "revision release failed",
        ),
        None => outcome,
    }
}

fn write_projected_risum(
    output: &mut impl Write,
    mut module: Value,
    occurrences: Option<&[OwnerManifestEntry]>,
    reader: &RevisionReadLease,
    repository: &PayloadCas,
    job: &JobControl,
) -> Result<u64, NativeJobError> {
    let object = module
        .as_object_mut()
        .ok_or_else(|| invalid_input("pinned root module must be an object"))?;
    // `exportModuleLegacy` writes the frames of `assets ?? []`.
    let assets = object
        .get_mut("assets")
        .filter(|value| !value.is_null())
        .map(|value| {
            value
                .as_array_mut()
                .ok_or_else(|| invalid_input("pinned root module assets must be an array"))
        })
        .transpose()?;
    let asset_count = assets.as_ref().map_or(0, |assets| assets.len());
    if asset_count > 50_000 {
        return Err(NativeJobError::too_large("module", "RISUM asset count exceeds the importer limit"));
    }
    if let Some(occurrences) = occurrences {
        if occurrences.len() != asset_count {
            return Err(invalid_input("RISUM owner occurrence count changed"));
        }
    }
    let mut descriptors = Vec::with_capacity(asset_count);
    if let Some(assets) = assets {
        for (position, asset) in assets.iter_mut().enumerate() {
            let tuple = asset
                .as_array_mut()
                .ok_or_else(|| invalid_input(format!("RISUM asset {position} must be a tuple")))?;
            // `exportModuleLegacy` reads only the key, `tuple[1]`, and writes the rest as stored.
            if tuple.len() < 2 {
                return Err(invalid_input(format!(
                    "RISUM asset {position} tuple is incomplete"
                )));
            }
            let key = tuple[1]
                .as_str()
                .ok_or_else(|| invalid_input(format!("RISUM asset {position} key is invalid")))?
                .to_owned();
            let alias = export::pinned_asset_alias(&reader.connection, &reader.target, &key)
                .map_err(store_error)?;
            let (hash, size) =
                if let Some(occurrence) = occurrences.and_then(|entries| entries.get(position)) {
                    if occurrence.tuple[1] != key {
                        return Err(invalid_input(
                            "RISUM occurrence does not match its positional key",
                        ));
                    }
                    let hash = occurrence
                        .payload_hash
                        .map(hex::encode)
                        .ok_or_else(|| invalid_input("RISUM occurrence has no native payload"))?;
                    let size = repository
                        .stat_available_object(&hash)
                        .map_err(io_error)?
                        .ok_or_else(|| invalid_input("RISUM occurrence payload is missing"))?;
                    (hash, size)
                } else {
                    let hash = alias
                        .object_hash
                        .ok_or_else(|| invalid_input("RISUM asset has no native payload"))?;
                    let size = u64::try_from(alias.size)
                        .map_err(|_| invalid_input("RISUM asset size is invalid"))?;
                    (hash, size)
                };
            if size > 64 * 1024 * 1024 {
                return Err(NativeJobError::too_large("module", "RISUM asset exceeds the importer per-frame limit"));
            }
            descriptors.push((hash, size));
            tuple[1] = Value::String(String::new());
        }
    }
    let metadata = match ported_metadata(&module, MAX_MAPPED_CARD_BYTES) {
        Some(metadata) => metadata,
        None => {
            // `exportModuleLegacy` writes `assets ?? []`, the key added last where it is absent.
            if let Some(object) = module.as_object_mut() {
                if matches!(object.get("assets"), None | Some(Value::Null)) {
                    object.insert("assets".to_owned(), Value::Array(Vec::new()));
                }
            }
            serde_json::to_vec_pretty(&json!({"module": module, "type": "risuModule"}))
                .map_err(|error| invalid_input(error.to_string()))?
        }
    };
    if metadata.len() > crate::import_export_jobs::MAX_CONTENT_METADATA_BYTES {
        return Err(NativeJobError::too_large("module", "RISUM metadata exceeds the importer limit"));
    }
    output.write_all(&[111, 0]).map_err(io_error)?;
    output
        .write_all(&(metadata.len() as u32).to_le_bytes())
        .map_err(io_error)?;
    write_rpack(output, &metadata, &|| job.is_cancel_requested())?;
    let mut aggregate = 0_u64;
    for (position, (hash, size)) in descriptors.iter().enumerate() {
        aggregate = aggregate
            .checked_add(*size)
            .ok_or_else(|| invalid_input("RISUM aggregate size overflow"))?;
        if aggregate > 10 * 1024 * 1024 * 1024 {
            return Err(NativeJobError::too_large("module", "RISUM assets exceed the aggregate importer limit"));
        }
        output.write_all(&[1]).map_err(io_error)?;
        output
            .write_all(&u32::try_from(*size).unwrap().to_le_bytes())
            .map_err(io_error)?;
        write_verified_rpack_object(output, repository, hash, *size, job)?;
        job.set_progress(JobProgress {
            completed_bytes: aggregate,
            total_bytes: None,
            completed_items: (position + 1) as u64,
            total_items: Some(asset_count as u64),
        })
        .map_err(job_error)?;
    }
    output.write_all(&[0]).map_err(io_error)?;
    Ok(asset_count as u64)
}

/// The metadata frame `exportModuleLegacy` writes for the pinned module, built by `risunest-formats` from the module
/// `JSON.parse` reads from its `serde_json` text. `None` where the port declines (a text above `max_bytes`, an own
/// `__proto__` key, asset tuples TS reads with JS semantics), and the job writes its own layout.
fn ported_metadata(module: &Value, max_bytes: usize) -> Option<Vec<u8>> {
    let mut module_text = BoundedText { bytes: Vec::new(), max_bytes };
    serde_json::to_writer(&mut module_text, module).ok()?;
    let module_text = String::from_utf8(module_text.bytes).ok()?;
    let module_text = module_text.as_str();
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("risum-metadata".into())
            .stack_size(MAPPING_STACK_BYTES)
            .spawn_scoped(scope, || {
                let budget = Budget::new(MAPPING_LIMITS, Arc::new(AtomicBool::new(false))).ok()?;
                let module = parse_json(module_text, &budget).ok()?;
                let text = risum_metadata_text(&module, &budget).ok()?;
                Some(text.as_unicode()?.as_bytes().to_vec())
            })
            .ok()?
            .join()
            .ok()
            .flatten()
    })
}

/// The bytes written to it, refusing a write past `max_bytes`, so a module above the cutoff is not written out in full.
struct BoundedText {
    bytes: Vec<u8>,
    max_bytes: usize,
}

impl Write for BoundedText {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        if self.bytes.len() + data.len() > self.max_bytes {
            return Err(std::io::Error::other("RISUM module text exceeds the port's cutoff"));
        }
        self.bytes.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn write_verified_rpack_object(
    output: &mut impl Write,
    repository: &PayloadCas,
    hash: &str,
    size: u64,
    job: &JobControl,
) -> Result<(), NativeJobError> {
    let mut input = repository
        .open_available_object(hash)
        .map_err(io_error)?
        .ok_or_else(|| invalid_input("RISUM payload is missing"))?;
    let mut copied = 0_u64;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        if job.is_cancel_requested() {
            return Err(cancelled("RISUM export cancelled while writing an asset"));
        }
        let read = input.read(&mut buffer).map_err(io_error)?;
        if read == 0 {
            break;
        }
        copied = copied
            .checked_add(read as u64)
            .ok_or_else(|| invalid_input("RISUM asset size overflow"))?;
        if copied > size {
            return Err(invalid_input("RISUM asset size changed"));
        }
        hasher.update(&buffer[..read]);
        for byte in &mut buffer[..read] {
            *byte = RPACK_MAP[*byte as usize];
        }
        output.write_all(&buffer[..read]).map_err(io_error)?;
    }
    if copied != size || hex::encode(hasher.finalize()) != hash {
        return Err(invalid_input("RISUM asset payload hash changed"));
    }
    Ok(())
}

fn destination_error(error: destination::DestinationWriteError) -> NativeJobError {
    destination_error_with(
        error,
        "RISUM source is invalid",
        "RISUM destination is invalid",
        "RISUM export cancelled before destination replacement",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asset_repository::{owner_manifest_codec, PayloadCas};
    use crate::import_export_jobs::{parse_risum, ImportLimits, JobStaging};
    use crate::native_file_jobs::{JobKind, JobRegistry};
    use crate::persistent_store::{
        AssetAlias, AssetOwnerHead, AssetOwnerLocator,
        PersistentStore, StoreError,
    };
    use risunest_ir::stage::StageError;
    use serde_json::json;
    use std::cell::Cell;
    use std::io::Cursor;
    use tempfile::TempDir;

    const EXPORT_CASES: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../tests/fixtures/rust-core/formats/risum-export.cases.json"
    ));
    const EXPORT_EXPECTED: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../tests/fixtures/rust-core/formats/risum-export.expected.json"
    ));

    #[test]
    fn writes_positional_frames_with_tuple_tails_duplicates_and_zero_bytes() {
        let module = json!({
            "name": "Module",
            "id": "same-id",
            "assets": [
                ["first", "assets/shared.bin", "BIN", {"tail": 1}],
                ["second", "assets/shared.bin", "bin", null],
                ["empty", "assets/empty.dat", "dat", "tail"]
            ]
        });
        let payloads = vec![b"first".to_vec(), b"second".to_vec(), Vec::new()];
        let mut bytes = Vec::new();
        write_risum(&mut bytes, &module, &payloads, || false).unwrap();

        let directory = TempDir::new().unwrap();
        let staging = JobStaging::open(directory.path()).unwrap();
        let parsed = parse_risum(
            &mut Cursor::new(bytes),
            &staging,
            &ImportLimits {
                max_metadata_bytes: 8 * 1024 * 1024,
                max_payload_count: 50_000,
                max_payload_bytes: 50 * 1024 * 1024,
                max_aggregate_payload_bytes: 10 * 1024 * 1024 * 1024,
                max_container_entries: 4096,
                max_container_directory_bytes: 32 * 1024 * 1024,
                charx_probe_metadata_bytes: 8 * 1024 * 1024,
            },
            &|| false,
        )
        .unwrap();
        let assets = parsed.metadata["module"]["assets"].as_array().unwrap();
        assert_eq!(assets[0], json!(["first", "", "BIN", {"tail": 1}]));
        assert_eq!(assets[1], json!(["second", "", "bin", null]));
        assert_eq!(assets[2], json!(["empty", "", "dat", "tail"]));
        assert_eq!(
            std::fs::read(directory.path().join(&parsed.assets[0].payload.staged_name)).unwrap(),
            b"first"
        );
        assert_eq!(
            std::fs::read(directory.path().join(&parsed.assets[1].payload.staged_name)).unwrap(),
            b"second"
        );
        assert_eq!(
            std::fs::read(directory.path().join(&parsed.assets[2].payload.staged_name)).unwrap(),
            b""
        );
    }

    fn alias(key: &str, bytes: &[u8], cas: &PayloadCas) -> AssetAlias {
        let payload = cas.prepare_bytes(bytes).unwrap();
        AssetAlias {
            key: key.to_owned(),
            object_hash: Some(payload.content_hash),
            kind: "asset".to_owned(),
            size: payload.byte_size as i64,
            mime: "application/octet-stream".to_owned(),
            name: key.to_owned(),
            ext: "bin".to_owned(),
            inlay_type: None,
            width: None,
            height: None,
            metadata: json!({}),
        }
    }

    fn publication_fixture() -> (TempDir, PersistentStore, i64) {
        let directory = TempDir::new().unwrap();
        let mut store = PersistentStore::open(directory.path()).unwrap();
        let staging = store.replace_begin().unwrap().staging_id;
        store
            .replace_put_root(
                &staging,
                &json!({"modules": [{"name": "Module", "id": "module-0"}]}),
            )
            .unwrap();
        store.replace_put_presets(&staging, &[]).unwrap();
        let revision = store.replace_commit(&staging, Some(0)).unwrap().revision;
        (directory, store, revision)
    }

    #[test]
    fn risum_release_failure_prevents_desktop_and_handoff_publication_exactly_once() {
        for portable in [false, true] {
            let (directory, mut store, revision) = publication_fixture();
            let prepared = store.prepare_risu_save_export(revision).unwrap();
            let owned = directory.path().join("owned");
            let chosen = directory.path().join("chosen");
            let handoffs = directory.path().join("handoffs");
            fs::create_dir(&owned).unwrap();
            fs::create_dir(&chosen).unwrap();
            let destination = chosen.join("module.risum");
            fs::write(&destination, b"previous RISUM").unwrap();
            let job = JobRegistry::default()
                .create(JobKind::ExportRisuModule)
                .unwrap();
            let release_calls = Cell::new(0);

            let error = export_risu_module_with_release(
                prepared,
                0,
                &owned,
                &handoffs,
                (!portable).then_some(destination.as_path()),
                &job,
                |prepared| {
                    release_calls.set(release_calls.get() + 1);
                    prepared.release_reader()?;
                    Err(StoreError::Store {
                        message: "injected checkpoint failure".to_owned(),
                    })
                },
            )
            .unwrap_err();

            assert_eq!(error.code, "store-error");
            assert_eq!(release_calls.get(), 1);
            assert_eq!(fs::read(&destination).unwrap(), b"previous RISUM");
            assert!(!handoffs.exists());
        }
    }

    #[test]
    fn risum_cancellation_and_stale_revision_do_not_publish() {
        let (directory, mut store, revision) = publication_fixture();
        assert!(matches!(
            store.prepare_risu_save_export(revision + 1).err().unwrap(),
            StoreError::RevisionConflict { .. }
        ));
        let prepared = store.prepare_risu_save_export(revision).unwrap();
        let owned = directory.path().join("owned");
        let chosen = directory.path().join("chosen");
        fs::create_dir(&owned).unwrap();
        fs::create_dir(&chosen).unwrap();
        let destination = chosen.join("module.risum");
        fs::write(&destination, b"previous RISUM").unwrap();
        let job = JobRegistry::default()
            .create(JobKind::ExportRisuModule)
            .unwrap();
        job.request_cancel().unwrap();

        let error = export_risu_module(
            prepared,
            0,
            &owned,
            &directory.path().join("handoffs"),
            Some(&destination),
            &job,
        )
        .unwrap_err();
        assert_eq!(error.code, "cancelled");
        assert_eq!(fs::read(destination).unwrap(), b"previous RISUM");
    }

    #[test]
    fn native_risum_writer_uses_exact_index_and_distinct_occurrence_hashes() {
        let directory = TempDir::new().unwrap();
        let mut store = PersistentStore::open(directory.path()).unwrap();
        let cas = PayloadCas::new(directory.path()).unwrap();
        let shared = alias("assets/shared.bin", b"first", &cas);
        let second = cas.prepare_bytes(b"second").unwrap();
        let empty = alias("assets/empty.bin", b"", &cas);
        let entries = vec![
            owner_manifest_codec::OwnerManifestEntry {
                tuple: ["first".to_owned(), shared.key.clone(), "BIN".to_owned()],
                payload_hash: Some(
                    hex::decode(shared.object_hash.as_ref().unwrap())
                        .unwrap()
                        .try_into()
                        .unwrap(),
                ),
            },
            owner_manifest_codec::OwnerManifestEntry {
                tuple: ["second".to_owned(), shared.key.clone(), "bin".to_owned()],
                payload_hash: Some(
                    hex::decode(&second.content_hash)
                        .unwrap()
                        .try_into()
                        .unwrap(),
                ),
            },
            owner_manifest_codec::OwnerManifestEntry {
                tuple: ["empty".to_owned(), empty.key.clone(), "bin".to_owned()],
                payload_hash: Some(
                    hex::decode(empty.object_hash.as_ref().unwrap())
                        .unwrap()
                        .try_into()
                        .unwrap(),
                ),
            },
        ];
        let manifest = cas
            .prepare_bytes(&owner_manifest_codec::encode_owner_manifest(&entries).unwrap())
            .unwrap();
        let root = json!({"modules": [
            {"name": "wrong", "id": "module-0"},
            {"name": "selected", "id": "module-1", "assets": [
                ["first", shared.key, "BIN", {"tail": 1}],
                ["second", shared.key, "bin", null],
                ["empty", empty.key, "bin", "tail"]
            ]}
        ]});
        let staging = store.replace_begin().unwrap().staging_id;
        store.replace_put_root(&staging, &root).unwrap();
        store.replace_put_presets(&staging, &[]).unwrap();
        store
            .replace_put_asset_aliases(&staging, &[shared, empty])
            .unwrap();
        store
            .replace_put_asset_owner_heads(
                &staging,
                &[AssetOwnerHead::present(
                    AssetOwnerLocator::RootModuleAssets { module_id: "module-1".to_owned() },
                    manifest.content_hash,
                    3,
                )],
            )
            .unwrap();
        let revision = store.replace_commit(&staging, Some(0)).unwrap().revision;
        let prepared = store.prepare_risu_save_export(revision).unwrap();
        let owned = directory.path().join("owned");
        let chosen = directory.path().join("chosen");
        fs::create_dir(&owned).unwrap();
        fs::create_dir(&chosen).unwrap();
        let destination = chosen.join("module.risum");
        let job = JobRegistry::default()
            .create(JobKind::ExportRisuModule)
            .unwrap();
        export_risu_module(
            prepared,
            1,
            &owned,
            &directory.path().join("handoffs"),
            Some(&destination),
            &job,
        )
        .unwrap();
        let parse_root = directory.path().join("parse");
        fs::create_dir(&parse_root).unwrap();
        let staging = JobStaging::open(&parse_root).unwrap();
        let parsed = parse_risum(
            &mut fs::File::open(destination).unwrap(),
            &staging,
            &ImportLimits {
                max_metadata_bytes: 8 * 1024 * 1024,
                max_payload_count: 50_000,
                max_payload_bytes: 64 * 1024 * 1024,
                max_aggregate_payload_bytes: 10 * 1024 * 1024 * 1024,
                max_container_entries: 4096,
                max_container_directory_bytes: 32 * 1024 * 1024,
                charx_probe_metadata_bytes: 8 * 1024 * 1024,
            },
            &|| false,
        )
        .unwrap();
        assert_eq!(parsed.metadata["module"]["name"], "selected");
        assert_eq!(
            parsed.metadata["module"]["assets"][0][3],
            json!({"tail": 1})
        );
        assert_eq!(
            fs::read(parse_root.join(&parsed.assets[0].payload.staged_name)).unwrap(),
            b"first"
        );
        assert_eq!(
            fs::read(parse_root.join(&parsed.assets[1].payload.staged_name)).unwrap(),
            b"second"
        );
        assert_eq!(
            fs::read(parse_root.join(&parsed.assets[2].payload.staged_name)).unwrap(),
            b""
        );
    }

    /// The file the native job writes for `module`, the stored root module, from a store holding `payloads` by key.
    fn exported(module: &Value, payloads: &serde_json::Map<String, Value>) -> Vec<u8> {
        let directory = TempDir::new().unwrap();
        let mut store = PersistentStore::open(directory.path()).unwrap();
        let cas = PayloadCas::new(directory.path()).unwrap();
        let aliases = payloads
            .iter()
            .map(|(key, payload)| alias(key, &hex::decode(payload.as_str().unwrap()).unwrap(), &cas))
            .collect::<Vec<_>>();
        let staging = store.replace_begin().unwrap().staging_id;
        store.replace_put_root(&staging, &json!({"modules": [module]})).unwrap();
        store.replace_put_presets(&staging, &[]).unwrap();
        store.replace_put_asset_aliases(&staging, &aliases).unwrap();
        let revision = store.replace_commit(&staging, Some(0)).unwrap().revision;
        let prepared = store.prepare_risu_save_export(revision).unwrap();
        let owned = directory.path().join("owned");
        let chosen = directory.path().join("chosen");
        fs::create_dir(&owned).unwrap();
        fs::create_dir(&chosen).unwrap();
        let destination = chosen.join("module.risum");
        let job = JobRegistry::default()
            .create(JobKind::ExportRisuModule)
            .unwrap();
        export_risu_module(
            prepared,
            0,
            &owned,
            &directory.path().join("handoffs"),
            Some(&destination),
            &job,
        )
        .unwrap();
        fs::read(destination).unwrap()
    }

    /// The metadata frame of a RISUM file, decoded.
    fn metadata_text(file: &[u8]) -> String {
        let length = u32::from_le_bytes(file[2..6].try_into().unwrap()) as usize;
        let mut decode = [0_u8; 256];
        for (byte, encoded) in RPACK_MAP[..256].iter().enumerate() {
            decode[*encoded as usize] = byte as u8;
        }
        String::from_utf8(file[6..6 + length].iter().map(|byte| decode[*byte as usize]).collect()).unwrap()
    }

    /// Replays `tests/fixtures/rust-core/formats/risum-export.*.json`, the files `exportModuleLegacy` writes, recorded
    /// by `benchmarks/rust-core-oracles/formats-risum-export.oracle.test.ts`. Where the port builds the metadata, the
    /// native file is TS's byte for byte; where it declines, it is the layout the job wrote before the port, with
    /// `assets ?? []` as TS writes it.
    #[test]
    fn native_risum_export_writes_the_file_ts_writes() {
        let digest: String = Sha256::digest(EXPORT_CASES)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let cases: Value = serde_json::from_slice(EXPORT_CASES).unwrap();
        let expected: Value = serde_json::from_slice(EXPORT_EXPECTED).unwrap();
        assert_eq!(expected["fixtureSha256"], json!(digest), "regenerate the expected file");
        let (cases, recorded) = (
            cases["cases"].as_array().unwrap(),
            expected["cases"].as_array().unwrap(),
        );
        assert_eq!(cases.len(), recorded.len());
        let (mut ported, mut changed, mut declined) = (0, 0, 0);
        for (case, record) in cases.iter().zip(recorded) {
            let id = case["id"].as_str().unwrap();
            assert_eq!(case["id"], record["id"]);
            let (module, payloads) = (&case["module"], case["payloads"].as_object().unwrap());
            let result = &record["result"];
            let file = exported(module, payloads);
            // What the job wrote before the port, from the same module and payloads.
            let frames = module["assets"].as_array().map_or_else(Vec::new, |assets| {
                assets
                    .iter()
                    .map(|tuple| hex::decode(payloads[tuple[1].as_str().unwrap()].as_str().unwrap()).unwrap())
                    .collect()
            });
            let layout = |module: &Value| {
                let mut bytes = Vec::new();
                write_risum(&mut bytes, module, &frames, || false).map(|()| bytes).ok()
            };
            let own = layout(module);
            if case["native"] == "ported" {
                assert_eq!(metadata_text(&file), result["metadata"].as_str().unwrap(), "{id}");
                assert_eq!(hex::encode(&file), result["file"].as_str().unwrap(), "{id}");
                ported += 1;
                changed += usize::from(own.as_ref() != Some(&file));
            } else {
                let budget = Budget::new(MAPPING_LIMITS, Arc::new(AtomicBool::new(false))).unwrap();
                let parsed = parse_json(&serde_json::to_string(module).unwrap(), &budget).unwrap();
                let reason = risum_metadata_text(&parsed, &budget).map(|_| ()).unwrap_err();
                assert!(
                    matches!(&reason, StageError::Unsupported(name) if json!(name) == case["native"]["declined"]),
                    "{id}: {reason:?}"
                );
                // That layout, of the module with `assets ?? []`.
                let mut fallback = module.clone();
                if matches!(fallback.get("assets"), None | Some(Value::Null)) {
                    fallback["assets"] = json!([]);
                }
                assert_eq!(Some(&file), layout(&fallback).as_ref(), "{id}");
                declined += 1;
            }
        }
        // Where TS writes otherwise than the job did before, the ported file is TS's.
        assert!(ported >= 11 && changed >= 6 && declined >= 3, "{ported} {changed} {declined}");
    }

    #[test]
    fn native_risum_metadata_is_ported_up_to_its_size_cutoff() {
        let (cases, expected): (Value, Value) = (
            serde_json::from_slice(EXPORT_CASES).unwrap(),
            serde_json::from_slice(EXPORT_EXPECTED).unwrap(),
        );
        let module = &cases["cases"][0]["module"];
        let length = serde_json::to_string(module).unwrap().len();
        let metadata = expected["cases"][0]["result"]["metadata"].as_str().unwrap();
        assert_eq!(ported_metadata(module, length).as_deref(), Some(metadata.as_bytes()));
        assert_eq!(ported_metadata(module, length - 1), None);
    }
}
