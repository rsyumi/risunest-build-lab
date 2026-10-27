//! Account cold items for an official snapshot restore. Staging reads an item
//! when it meets the reference, under the retry rules of the asset reads, and
//! the item is inflated into the job directory for the restore reader to parse.
use super::download::{latin1_header, AccountReader};
use super::scheduler::{Exchange, Outcome, Pause, Scheduler, Settled, StopReason};
use crate::native_file_jobs::official_snapshot::{await_network_operation, OfficialSnapshotCredential, NETWORK_IDLE_TIMEOUT};
use crate::native_file_jobs::restore::{is_zlib_header, ColdPayloadUnavailable};
use crate::native_file_jobs::{JobControl, NativeJobError};
use flate2::read::{DeflateDecoder, MultiGzDecoder, ZlibDecoder};
use reqwest::header::{ACCEPT_ENCODING, CONTENT_ENCODING, RETRY_AFTER};
use risunest_network_log::reqwest::RequestBuilderExt;
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, Read as _, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const TICK: Duration = Duration::from_millis(150);

pub(crate) struct ColdSource<'a> {
    reader: AccountReader,
    job: &'a JobControl,
    limit: u64,
    state: Mutex<State>,
    /// Items read and handed to staging, which the restore counts as progress.
    items_read: Arc<AtomicU64>,
}

struct State {
    scheduler: Scheduler,
    sequence: u64,
    /// The last payload handed out; staging has parsed it by the next lookup.
    previous: Option<PathBuf>,
    unavailable: HashMap<String, ColdPayloadUnavailable>,
    fatal: Option<NativeJobError>,
}

enum Fetched {
    Present,
    Unavailable(ColdPayloadUnavailable),
}

enum Read {
    Body(Fetched),
    Ended(Exchange),
    Cancelled,
}

impl<'a> ColdSource<'a> {
    pub(crate) fn new(
        base_url: &str,
        credential: &OfficialSnapshotCredential,
        directory: &Path,
        job: &'a JobControl,
        limit: u64,
    ) -> Result<Self, NativeJobError> {
        // Items are read one at a time, and a rejected credential ends the job so
        // the renderer can sign in again and restart the restore.
        let scheduler = Scheduler::new(1, Arc::new(Mutex::new(Pause::default())), usize::MAX, false, Box::new(super::jitter));
        Ok(Self {
            reader: AccountReader::new(base_url, credential, directory)?,
            job,
            limit,
            state: Mutex::new(State { scheduler, sequence: 0, previous: None, unavailable: HashMap::new(), fatal: None }),
            items_read: Arc::default(),
        })
    }

    /// The count of items read so far, for the restore's progress.
    pub(crate) fn items_read(&self) -> Arc<AtomicU64> {
        Arc::clone(&self.items_read)
    }

    /// The inflated item, or `None` when the account holds no readable item for
    /// the key. An error ends the restore, and every later lookup repeats it.
    pub(crate) fn lookup(&self, key: &str) -> Result<Option<PathBuf>, NativeJobError> {
        let mut state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(error) = &state.fatal {
            return Err(error.clone());
        }
        // A reader that still holds the file keeps reading it after removal.
        if let Some(previous) = state.previous.take() {
            let _ = fs::remove_file(previous);
        }
        if state.unavailable.contains_key(key) {
            return Ok(None);
        }
        let result = self.fetch(&mut state, key);
        if let Err(error) = &result {
            state.fatal = Some(error.clone());
        }
        result
    }

    /// Why a key that `lookup` did not find is unreadable; `None` means missing.
    pub(crate) fn unavailable(&self, key: &str) -> Option<ColdPayloadUnavailable> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).unavailable.get(key).copied()
    }

    /// The error that ended the restore, if a lookup ended it.
    pub(crate) fn fatal(&self) -> Option<NativeJobError> {
        self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).fatal.clone()
    }

    fn fetch(&self, state: &mut State, key: &str) -> Result<Option<PathBuf>, NativeJobError> {
        state.scheduler.receive(1);
        loop {
            if self.job.is_cancel_requested() {
                return Err(cancelled());
            }
            let now = super::now_ms();
            let Some(attempt) = state.scheduler.next(now, true) else {
                if let Some(reason) = state.scheduler.reason() {
                    return Err(stopped(reason));
                }
                let wait = state.scheduler.wake_at(true).map_or(TICK, |at| TICK.min(Duration::from_millis(at.saturating_sub(now))));
                std::thread::sleep(wait);
                continue;
            };
            state.sequence += 1;
            let path = self.reader.directory.join(format!("cold-{}.json", state.sequence));
            let (exchange, fetched) = match tauri::async_runtime::block_on(self.read(key, &path)) {
                Read::Cancelled => {
                    state.scheduler.abandon(attempt);
                    return Err(cancelled());
                }
                Read::Body(fetched) => (Exchange::Body, Some(fetched)),
                Read::Ended(exchange) => (exchange, None),
            };
            match (state.scheduler.finish(attempt, exchange, super::now_ms()), fetched) {
                (Outcome::Install, Some(Fetched::Present)) => {
                    state.previous = Some(path.clone());
                    self.items_read.fetch_add(1, Ordering::AcqRel);
                    return Ok(Some(path));
                }
                (Outcome::Install, Some(Fetched::Unavailable(reason))) => {
                    state.unavailable.insert(key.to_owned(), reason);
                    return Ok(None);
                }
                (Outcome::Settled(Settled::Missing), _) => return Ok(None),
                (Outcome::Settled(_), _) => {
                    if let Some(reason) = state.scheduler.reason() {
                        return Err(stopped(reason));
                    }
                    state.unavailable.insert(key.to_owned(), ColdPayloadUnavailable::Damaged);
                    return Ok(None);
                }
                (Outcome::Unsettled, _) => {
                    return Err(stopped(state.scheduler.reason().unwrap_or(StopReason::ReauthenticationNeeded)));
                }
                (Outcome::Requeued | Outcome::Install, _) => {}
            }
        }
    }

    async fn read(&self, key: &str, path: &Path) -> Read {
        let Some(key_header) = latin1_header(key) else {
            return Read::Ended(Exchange::Permanent);
        };
        let request = self
            .reader
            .client
            .get(format!("{}/hub/account/coldstorage", self.reader.base_url))
            .headers(self.reader.credential.clone())
            .header("x-risu-key", key_header)
            .header(ACCEPT_ENCODING, "identity")
            .send_logged("account-cold-read");
        let response = match self.network(request).await {
            Ok(response) => response,
            Err(read) => return read,
        };
        let status = response.status().as_u16();
        match status {
            200 => {}
            401 | 403 => return Read::Ended(Exchange::Rejected),
            404 => return Read::Ended(Exchange::Missing),
            408 | 429 | 500..=u16::MAX => {
                return Read::Ended(Exchange::Retryable {
                    status: Some(status),
                    retry_after: response.headers().get(RETRY_AFTER).and_then(|value| value.to_str().ok()).map(str::to_owned),
                })
            }
            _ => return Read::Ended(Exchange::Permanent),
        }
        if response.headers().get(CONTENT_ENCODING).is_some_and(|value| !value.as_bytes().eq_ignore_ascii_case(b"identity")) {
            return Read::Body(Fetched::Unavailable(ColdPayloadUnavailable::Damaged));
        }
        let compressed = path.with_extension("part");
        let read = self.inflate(response, &compressed, path).await;
        let _ = fs::remove_file(&compressed);
        if !matches!(read, Read::Body(Fetched::Present)) {
            let _ = fs::remove_file(path);
        }
        read
    }

    async fn network<F, T>(&self, operation: F) -> Result<T, Read>
    where
        F: std::future::Future<Output = Result<T, reqwest::Error>>,
    {
        await_network_operation(operation, self.job, NETWORK_IDLE_TIMEOUT, "Official account cold item read was cancelled")
            .await
            .map_err(|error| {
                if error.code == "cancelled" {
                    Read::Cancelled
                } else {
                    Read::Ended(Exchange::Retryable { status: None, retry_after: None })
                }
            })
    }

    /// Spools the response, then inflates gzip, zlib or raw deflate the way the
    /// renderer's account reader does, within the decoded block limit.
    async fn inflate(&self, mut response: reqwest::Response, compressed: &Path, path: &Path) -> Read {
        let Ok(mut file) = OpenOptions::new().write(true).create_new(true).open(compressed) else {
            return Read::Ended(Exchange::Local);
        };
        let mut received = 0u64;
        loop {
            let chunk = match self.network(response.chunk()).await {
                Ok(Some(chunk)) => chunk,
                Ok(None) => break,
                Err(read) => return read,
            };
            received += chunk.len() as u64;
            if received > self.limit {
                return Read::Body(Fetched::Unavailable(ColdPayloadUnavailable::TooLarge));
            }
            if file.write_all(&chunk).is_err() {
                return Read::Ended(Exchange::Local);
            }
        }
        drop(file);
        let Ok(mut source) = File::open(compressed) else {
            return Read::Ended(Exchange::Local);
        };
        let mut head = Vec::with_capacity(2);
        if (&mut source).take(2).read_to_end(&mut head).is_err() {
            return Read::Ended(Exchange::Local);
        }
        if head.len() < 2 {
            return Read::Body(Fetched::Unavailable(ColdPayloadUnavailable::Damaged));
        }
        let gzip = head == [0x1f, 0x8b];
        let zlib = is_zlib_header(&head);
        let source = std::io::Cursor::new(head).chain(BufReader::new(source));
        let mut decoder: Box<dyn std::io::Read> = if gzip {
            Box::new(MultiGzDecoder::new(source))
        } else if zlib {
            Box::new(ZlibDecoder::new(source))
        } else {
            Box::new(DeflateDecoder::new(source))
        };
        let Ok(mut output) = OpenOptions::new().write(true).create_new(true).open(path) else {
            return Read::Ended(Exchange::Local);
        };
        let mut buffer = vec![0u8; 64 * 1024];
        let mut decoded = 0u64;
        loop {
            if self.job.is_cancel_requested() {
                return Read::Cancelled;
            }
            let read = match decoder.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => read,
                Err(_) => return Read::Body(Fetched::Unavailable(ColdPayloadUnavailable::Damaged)),
            };
            decoded += read as u64;
            if decoded > self.limit {
                return Read::Body(Fetched::Unavailable(ColdPayloadUnavailable::TooLarge));
            }
            if output.write_all(&buffer[..read]).is_err() {
                return Read::Ended(Exchange::Local);
            }
        }
        drop(output);
        // An item is one JSON value of any kind, as the renderer's reader accepts.
        let Ok(file) = File::open(path) else {
            return Read::Ended(Exchange::Local);
        };
        let mut deserializer = serde_json::Deserializer::from_reader(BufReader::new(file));
        match <serde::de::IgnoredAny as serde::Deserialize>::deserialize(&mut deserializer).and_then(|_| deserializer.end()) {
            Ok(()) => Read::Body(Fetched::Present),
            Err(error) if error.is_io() => Read::Ended(Exchange::Local),
            Err(_) => Read::Body(Fetched::Unavailable(ColdPayloadUnavailable::Damaged)),
        }
    }
}

fn cancelled() -> NativeJobError {
    NativeJobError::new("cancelled", "Official account cold item read was cancelled")
}

fn stopped(reason: StopReason) -> NativeJobError {
    match reason {
        StopReason::Authentication | StopReason::ReauthenticationNeeded | StopReason::AuthWarning => {
            NativeJobError::new("reauthentication-needed", "Official account authorization failed")
        }
        StopReason::Local | StopReason::LibraryChanged => {
            NativeJobError::new("store-error", "Official account cold item could not be stored")
        }
        StopReason::Service => NativeJobError::new("network-error", "Official account cold storage is not responding"),
    }
}
