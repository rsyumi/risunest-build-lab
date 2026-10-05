use super::error::{
    cancelled, destination_error_with, finish_with_release, io_error, job_error, store_error,
};
use super::{JobControl, JobPhase, JobProgress, JobResultSummary, NativeJobError};
use crate::persistent_store::export::destination;
use crate::persistent_store::{PreparedRisuSaveExport, StoreError};
use rusqlite::{params, Connection};
use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Serialize, Serializer};
use serde_json::ser::PrettyFormatter;
use serde_json::{Map, Value};
use std::cell::RefCell;
use std::fs::{self, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::Path;
use uuid::Uuid;

pub(crate) const PREFIX: &str = "risu-dataset-";
pub(crate) const SUFFIX: &str = ".json";
const COPY_BUFFER_BYTES: usize = 64 * 1024;
const CANCELLED_WHILE_WRITING: &str = "dataset export cancelled while writing";

/// Writes the renderer's "Export Save as Dataset" file from a leased revision,
/// reading one character and one conversation at a time.
pub(crate) fn export_dataset(
    mut prepared: PreparedRisuSaveExport,
    owned_directory: &Path,
    handoff_directory: &Path,
    destination_path: Option<&Path>,
    job: &JobControl,
) -> Result<JobResultSummary, NativeJobError> {
    let mut released = false;
    let outcome = export_with_reader(
        &mut prepared,
        &mut released,
        owned_directory,
        handoff_directory,
        destination_path,
        job,
    );
    if released {
        return outcome;
    }
    finish_with_release(outcome, prepared.release_reader(), "revision release failed")
}

fn export_with_reader(
    prepared: &mut PreparedRisuSaveExport,
    released: &mut bool,
    owned_directory: &Path,
    handoff_directory: &Path,
    destination_path: Option<&Path>,
    job: &JobControl,
) -> Result<JobResultSummary, NativeJobError> {
    if job.is_cancel_requested() {
        return Err(cancelled("dataset export cancelled before writing"));
    }
    job.start(JobPhase::WritingExport).map_err(job_error)?;
    let source = owned_directory.join("dataset.json");
    let character_count = {
        let reader = prepared.reader().map_err(store_error)?;
        let connection = &reader.connection;
        let generation = reader.target.generation.as_str();
        let character_count = connection
            .query_row(
                "SELECT COUNT(*) FROM characters WHERE generation = ?1 AND archived_object IS NULL",
                [generation],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|error| store_error(error.into()))?;
        let character_count = u64::try_from(character_count)
            .map_err(|_| NativeJobError::new("store-error", "character count is invalid"))?;
        job.set_progress(JobProgress {
            completed_bytes: 0,
            total_bytes: None,
            completed_items: 0,
            total_items: Some(character_count),
        })
        .map_err(job_error)?;
        let file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&source)
            .map_err(io_error)?;
        let mut output = BufWriter::with_capacity(COPY_BUFFER_BYTES, CancellableWriter { inner: file, job });
        write_dataset(connection, generation, character_count, &mut output, job)?;
        let file = output
            .into_inner()
            .map_err(|error| write_failure(job, error.into_error()))?
            .inner;
        file.sync_all().map_err(io_error)?;
        character_count
    };
    let source_bytes = fs::metadata(&source).map_err(io_error)?.len();
    if job.is_cancel_requested() {
        return Err(cancelled("dataset export cancelled before destination publication"));
    }
    *released = true;
    prepared.release_reader().map_err(store_error)?;
    job.set_phase(JobPhase::PublishingDestination).map_err(job_error)?;
    let (destination_root, destination_path, handoff_path) = match destination_path {
        Some(destination_path) => (
            destination_path.parent().ok_or_else(|| {
                NativeJobError::new(
                    "invalid-destination",
                    "dataset destination directory is unavailable",
                )
            })?,
            destination_path.to_owned(),
            None,
        ),
        None => {
            fs::create_dir_all(handoff_directory).map_err(io_error)?;
            let path = handoff_directory.join(format!("{PREFIX}{}{SUFFIX}", Uuid::new_v4()));
            (
                handoff_directory,
                path.clone(),
                Some(path.to_string_lossy().into_owned()),
            )
        }
    };
    let phase_failure = RefCell::new(None);
    let published = destination::write_dataset_destination_controlled(
        owned_directory,
        &source,
        destination_root,
        &destination_path,
        || job.is_cancel_requested() || phase_failure.borrow().is_some(),
        |progress| {
            if let Err(error) = job.set_progress(JobProgress {
                completed_bytes: progress.copied_bytes,
                total_bytes: Some(source_bytes),
                completed_items: character_count,
                total_items: Some(character_count),
            }) {
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
        phase_failure.into_inner().map(job_error).unwrap_or_else(|| {
            destination_error_with(
                error,
                "dataset source is invalid",
                "dataset destination is invalid",
                "dataset export cancelled before destination replacement",
            )
        })
    })?;

    Ok(JobResultSummary {
        export_exclusions: None,
        revision: prepared.revision,
        source_bytes: published.bytes,
        source_sha256: published.sha256,
        source_fingerprint_kind: crate::native_file_jobs::SourceFingerprintKind::WholeFileSha256,
        character_count,
        preset_count: 0,
        warning_codes: Vec::new(),
        handoff_path,
        publication: None,
    })
}

struct CancellableWriter<'a, W> {
    inner: W,
    job: &'a JobControl,
}

impl<W: Write> Write for CancellableWriter<'_, W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        if self.job.is_cancel_requested() {
            return Err(io::Error::other(CANCELLED_WHILE_WRITING));
        }
        self.inner.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn write_failure(job: &JobControl, error: io::Error) -> NativeJobError {
    if job.is_cancel_requested() {
        cancelled(CANCELLED_WHILE_WRITING)
    } else {
        io_error(error)
    }
}

// Four-space indentation, as `JSON.stringify(dataset, null, 4)` writes it.
fn write_dataset(
    connection: &Connection,
    generation: &str,
    character_count: u64,
    output: &mut impl Write,
    job: &JobControl,
) -> Result<(), NativeJobError> {
    let failure = RefCell::new(None);
    let dataset = Dataset {
        connection,
        generation,
        character_count,
        job,
        failure: &failure,
    };
    let mut serializer =
        serde_json::Serializer::with_formatter(output, PrettyFormatter::with_indent(b"    "));
    let Err(error) = dataset.serialize(&mut serializer) else {
        return Ok(());
    };
    if job.is_cancel_requested() {
        return Err(cancelled(CANCELLED_WHILE_WRITING));
    }
    if let Some(failure) = failure.into_inner() {
        return Err(failure);
    }
    if error.is_io() {
        return Err(io_error(error.into()));
    }
    Err(NativeJobError::new("store-error", error.to_string()))
}

struct Dataset<'a> {
    connection: &'a Connection,
    generation: &'a str,
    character_count: u64,
    job: &'a JobControl,
    failure: &'a RefCell<Option<NativeJobError>>,
}

impl Dataset<'_> {
    // Serde only carries a message, so the job error waits here for the caller.
    fn fail<E: serde::ser::Error>(&self, error: NativeJobError) -> E {
        let message = error.message.clone();
        self.failure.borrow_mut().get_or_insert(error);
        E::custom(message)
    }

    fn store<T, E: serde::ser::Error>(
        &self,
        result: Result<T, impl Into<StoreError>>,
    ) -> Result<T, E> {
        result.map_err(|error| self.fail(store_error(error.into())))
    }
}

// One entry per conversation of every character, in
// library order, with the fields `exportAsDataset` reads.
impl Serialize for Dataset<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut entries = serializer.serialize_seq(None)?;
        let mut characters = self.store(self.connection.prepare(
            "SELECT character_id, detail FROM characters
             WHERE generation = ?1 AND archived_object IS NULL ORDER BY configured_index ASC",
        ))?;
        let mut rows = self.store(characters.query([self.generation]))?;
        let mut completed_items = 0;
        while let Some(row) = self.store(rows.next())? {
            let character_id: String = self.store(row.get(0))?;
            let detail: String = self.store(row.get(1))?;
            let detail: Value = self.store(serde_json::from_str(&detail))?;
            let Value::Object(detail) = detail else {
                return Err(self.fail(store_error(StoreError::Store {
                    message: "Character detail must be an object".to_owned(),
                })));
            };
            {
                let mut conversations = self.store(self.connection.prepare_cached(
                    "SELECT conversation_id FROM conversations
                     WHERE generation = ?1 AND character_id = ?2 ORDER BY configured_index ASC",
                ))?;
                let mut ids = self.store(conversations.query(params![self.generation, character_id]))?;
                while let Some(id) = self.store(ids.next())? {
                    let conversation_id: String = self.store(id.get(0))?;
                    entries.serialize_element(&Entry {
                        detail: &detail,
                        messages: Messages {
                            dataset: self,
                            character_id: &character_id,
                            conversation_id: &conversation_id,
                        },
                    })?;
                }
            }
            completed_items += 1;
            self.job
                .set_progress(JobProgress {
                    completed_bytes: 0,
                    total_bytes: None,
                    completed_items,
                    total_items: Some(self.character_count),
                })
                .map_err(|error| self.fail(job_error(error)))?;
        }
        entries.end()
    }
}

struct Entry<'a> {
    detail: &'a Map<String, Value>,
    messages: Messages<'a>,
}

// Absent fields are left out, as `JSON.stringify` leaves out `undefined`.
impl Serialize for Entry<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut entry = serializer.serialize_map(None)?;
        if let Some(name) = self.detail.get("name") {
            entry.serialize_entry("name", name)?;
        }
        if let Some(description) = self.detail.get("desc") {
            entry.serialize_entry("description", description)?;
        }
        entry.serialize_entry("chats", &self.messages)?;
        if let Some(lorebook) = self.detail.get("globalLore") {
            entry.serialize_entry("lorebook", lorebook)?;
        }
        entry.end()
    }
}

struct Messages<'a> {
    dataset: &'a Dataset<'a>,
    character_id: &'a str,
    conversation_id: &'a str,
}

impl Serialize for Messages<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let dataset = self.dataset;
        let mut statement = dataset.store(dataset.connection.prepare_cached(
            "SELECT value FROM messages
             WHERE generation = ?1 AND character_id = ?2 AND conversation_id = ?3
             ORDER BY message_index ASC",
        ))?;
        let mut rows = dataset.store(statement.query(params![
            dataset.generation,
            self.character_id,
            self.conversation_id
        ]))?;
        let mut messages = serializer.serialize_seq(None)?;
        while let Some(row) = dataset.store(rows.next())? {
            let message: String = dataset.store(row.get(0))?;
            let message: Value = dataset.store(serde_json::from_str(&message))?;
            messages.serialize_element(&message)?;
        }
        messages.end()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_file_jobs::{JobKind, JobRegistry};
    use crate::persistent_store::PersistentStore;
    use sha2::{Digest, Sha256};
    use tempfile::TempDir;

    const GOLDEN: &str =
        include_str!("../../../src/ts/storage/tests/fixtures/datasetExportGolden.json");

    struct Fixture {
        directory: TempDir,
        store: PersistentStore,
        revision: i64,
        dataset: Value,
    }

    fn fixture() -> Fixture {
        let golden: Value = serde_json::from_str(GOLDEN).unwrap();
        let directory = TempDir::new().unwrap();
        let mut store = PersistentStore::open(directory.path()).unwrap();
        let staging = store.replace_begin().unwrap().staging_id;
        store.replace_put_root(&staging, &serde_json::json!({})).unwrap();
        store.replace_put_presets(&staging, &[]).unwrap();
        store
            .replace_add_characters(&staging, golden["characters"].as_array().unwrap())
            .unwrap();
        let revision = store.replace_commit(&staging, Some(0)).unwrap().revision;
        Fixture {
            directory,
            store,
            revision,
            dataset: golden["dataset"].clone(),
        }
    }

    fn run(
        fixture: &mut Fixture,
        destination: Option<&Path>,
        job: &JobControl,
    ) -> Result<JobResultSummary, NativeJobError> {
        let prepared = fixture.store.prepare_risu_save_export(fixture.revision).unwrap();
        let owned = fixture.directory.path().join("owned");
        fs::create_dir_all(&owned).unwrap();
        export_dataset(
            prepared,
            &owned,
            &fixture.directory.path().join("handoffs"),
            destination,
            job,
        )
    }

    fn job() -> std::sync::Arc<JobControl> {
        JobRegistry::default().create(JobKind::ExportDataset).unwrap()
    }

    // The renderer export's loop over the library the store materializes.
    fn renderer_dataset(database: &Value) -> Value {
        let mut dataset = Vec::new();
        for character in database["characters"].as_array().unwrap() {
            for chat in character["chats"].as_array().unwrap() {
                let mut entry = Map::new();
                for (from, to) in [("name", "name"), ("desc", "description")] {
                    if let Some(value) = character.get(from) {
                        entry.insert(to.to_owned(), value.clone());
                    }
                }
                entry.insert("chats".to_owned(), chat["message"].clone());
                if let Some(lorebook) = character.get("globalLore") {
                    entry.insert("lorebook".to_owned(), lorebook.clone());
                }
                dataset.push(Value::Object(entry));
            }
        }
        Value::Array(dataset)
    }

    #[test]
    fn the_dataset_parses_like_the_renderer_export_of_the_shared_fixture() {
        let mut fixture = fixture();
        let chosen = fixture.directory.path().join("chosen");
        fs::create_dir(&chosen).unwrap();
        let destination = chosen.join("dataset.json");
        fs::write(&destination, b"previous export").unwrap();

        let result = run(&mut fixture, Some(&destination), &job()).unwrap();

        let text = fs::read_to_string(&destination).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), fixture.dataset);
        let materialized = fixture.store.materialize(Some(fixture.revision)).unwrap();
        assert_eq!(renderer_dataset(&materialized), fixture.dataset);
        assert!(text.starts_with("[\n    {\n        \"name\": \"Alice\",\n"), "{text}");
        assert_eq!(result.revision, fixture.revision);
        assert_eq!(result.character_count, 3);
        assert_eq!(result.handoff_path, None);
        assert_eq!(result.source_bytes, text.len() as u64);
        assert_eq!(result.source_sha256, hex::encode(Sha256::digest(text.as_bytes())));
    }

    #[test]
    fn a_library_without_conversations_exports_an_empty_list() {
        let directory = TempDir::new().unwrap();
        let mut store = PersistentStore::open(directory.path()).unwrap();
        let staging = store.replace_begin().unwrap().staging_id;
        store.replace_put_root(&staging, &serde_json::json!({})).unwrap();
        store.replace_put_presets(&staging, &[]).unwrap();
        let revision = store.replace_commit(&staging, Some(0)).unwrap().revision;
        let mut fixture = Fixture { directory, store, revision, dataset: Value::Null };
        let destination = fixture.directory.path().join("dataset.json");

        run(&mut fixture, Some(&destination), &job()).unwrap();

        assert_eq!(fs::read_to_string(&destination).unwrap(), "[]");
    }

    #[test]
    fn a_portable_export_returns_the_exact_managed_handoff_name() {
        let mut fixture = fixture();

        let result = run(&mut fixture, None, &job()).unwrap();

        let handoff = result.handoff_path.map(std::path::PathBuf::from).unwrap();
        assert_eq!(handoff.parent(), Some(fixture.directory.path().join("handoffs").as_path()));
        assert!(crate::native_file_jobs::handoff_name(&handoff, PREFIX, SUFFIX));
        let bytes = fs::read(&handoff).unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap(), fixture.dataset);
        assert_eq!(result.source_sha256, hex::encode(Sha256::digest(&bytes)));
    }

    #[test]
    fn a_cancelled_export_leaves_the_destination_and_releases_the_revision() {
        let mut fixture = fixture();
        let destination = fixture.directory.path().join("dataset.json");
        fs::write(&destination, b"previous export").unwrap();
        let job = job();
        job.request_cancel().unwrap();

        let error = run(&mut fixture, Some(&destination), &job).unwrap_err();

        assert_eq!(error.code, "cancelled");
        assert_eq!(fs::read(&destination).unwrap(), b"previous export");
        fixture.store.prepare_risu_save_export(fixture.revision).unwrap().release_reader().unwrap();
    }
}
