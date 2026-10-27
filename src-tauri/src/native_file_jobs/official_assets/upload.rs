//! Official account asset publication. Each pinned body streams from the
//! payload store into an account write under the scheduling rules of the asset
//! reads, and every settled asset comes back through a cursor with the key the
//! account stores it under.
use super::download::{latin1_header, validate_base_url};
use super::scheduler::{self, Attempt, Exchange, Outcome, Scheduler, StopReason};
use super::{jitter, now_ms, MAX_KEY_BYTES, PAGE, TICK};
use crate::native_file_jobs::error::job_error;
use crate::native_file_jobs::official_snapshot::{
    authenticated_headers, await_network_operation, bounded_response_bytes, OfficialSnapshotCredential,
    NETWORK_IDLE_TIMEOUT,
};
use crate::native_file_jobs::{
    JobControl, JobPhase, JobProgress, JobResultSummary, JobState, NativeFileJobState, NativeJobError,
    SourceFingerprintKind,
};
use crate::native_log::logged;
use crate::persistent_store::{PersistentStore, StoreResult};
use futures::stream::{FuturesUnordered, StreamExt};
use reqwest::header::{HeaderMap, HeaderValue, CONTENT_LENGTH, CONTENT_TYPE, RETRY_AFTER};
use risunest_network_log::reqwest::RequestBuilderExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tauri::State;
use tokio::io::{AsyncRead, ReadBuf};
use tokio_util::io::ReaderStream;

const MAX_ASSETS: usize = 1_000_000;
const MAX_KEYS_BYTES: usize = 64 * 1024 * 1024;
const MAX_HEADER_BYTES: usize = 4096;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PinnedAsset {
    /// The library alias that holds the body.
    pub(crate) local_key: String,
    /// The key the account stores it under.
    pub(crate) key: String,
}

pub(crate) struct PublicationRequest {
    pub(crate) base_url: String,
    pub(crate) credential: OfficialSnapshotCredential,
    pub(crate) account_id: String,
    pub(crate) session: Option<String>,
    pub(crate) concurrency: usize,
    pub(crate) root: PathBuf,
    /// Each account key with the payload object its alias names in the leased revision.
    pub(crate) uploads: Vec<(String, Option<String>)>,
    pub(crate) shared: PublicationHandle,
}

/// Checks a start request and returns the concurrency it asks for.
pub(crate) fn validate_publication_start(
    base_url: &str,
    account_id: &str,
    lease: &str,
    session: Option<&str>,
    assets: &[PinnedAsset],
    concurrency: Option<u8>,
) -> Result<usize, NativeJobError> {
    validate_base_url(base_url)?;
    let invalid = |message: &str| NativeJobError::new("invalid-input", message);
    if account_id.is_empty() || account_id.len() > 512 {
        return Err(invalid("Official asset publication requires an account"));
    }
    if !lease.starts_with("snapshot-") || lease.len() > MAX_HEADER_BYTES {
        return Err(invalid("Official asset publication revision lease is invalid"));
    }
    if session.is_some_and(|session| session.len() > MAX_HEADER_BYTES || HeaderValue::from_str(session).is_err()) {
        return Err(invalid("Official asset publication session is invalid"));
    }
    if assets.len() > MAX_ASSETS {
        return Err(invalid("Official asset publication has too many assets"));
    }
    let mut total = 0usize;
    for key in assets.iter().flat_map(|asset| [&asset.local_key, &asset.key]) {
        if key.len() > MAX_KEY_BYTES || !(key.starts_with("assets/") || key.starts_with("assets\\")) {
            return Err(invalid("Official asset publication accepts only asset keys"));
        }
        total += key.len();
    }
    if total > MAX_KEYS_BYTES {
        return Err(invalid("Official asset publication keys are too large"));
    }
    match concurrency {
        None => Ok(2),
        Some(value @ (1 | 2 | 4)) => Ok(usize::from(value)),
        Some(_) => Err(invalid("Official asset publication concurrency must be 1, 2 or 4")),
    }
}

/// The payload object each asset's alias names in the leased revision, which
/// keeps the bodies pinned while the job runs.
pub(crate) fn pinned_uploads(
    store: &PersistentStore,
    lease: &str,
    assets: &[PinnedAsset],
) -> StoreResult<(PathBuf, Vec<(String, Option<String>)>)> {
    let mut uploads = Vec::with_capacity(assets.len());
    for page in assets.chunks(PAGE) {
        let keys = page.iter().map(|asset| asset.local_key.clone()).collect::<Vec<_>>();
        let objects = store
            .read_asset_aliases_by_keys("asset", &keys, Some(lease))?
            .value
            .into_iter()
            .map(|alias| (alias.key, alias.object_hash))
            .collect::<HashMap<_, _>>();
        uploads.extend(page.iter().map(|asset| (asset.key.clone(), objects.get(&asset.local_key).cloned().flatten())));
    }
    Ok((store.repository_root().to_path_buf(), uploads))
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PublicationProgress {
    pub(crate) written: u64,
    pub(crate) failed: u64,
    pub(crate) bytes: u64,
    pub(crate) active: usize,
    pub(crate) limit: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) waiting_until: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) reason: Option<StopReason>,
    /// The account session the writes used, for the renderer to keep.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) session: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) warning: Option<String>,
    pub(crate) reload_session: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SettledUpload {
    index: usize,
    /// `None` when the write failed.
    replacement_key: Option<String>,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PublicationPage {
    entries: Vec<SettledUpload>,
    total: usize,
}

/// Every settled asset in the order it settled, as a job status field; equality
/// is identity, so cloning a status stays cheap.
#[derive(Clone, Default)]
pub(crate) struct PublicationHandle(Arc<Mutex<Vec<SettledUpload>>>);

impl PartialEq for PublicationHandle {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for PublicationHandle {}

impl std::fmt::Debug for PublicationHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("OfficialAssetPublication")
    }
}

fn settled(shared: &Mutex<Vec<SettledUpload>>) -> MutexGuard<'_, Vec<SettledUpload>> {
    shared.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn settled_page(state: &NativeFileJobState, job_id: &str, offset: usize, limit: usize) -> Result<PublicationPage, NativeJobError> {
    if limit == 0 || limit > PAGE {
        return Err(NativeJobError::new("invalid-input", "Official asset settled page must hold 1 to 512 entries"));
    }
    let job = state
        .registry
        .lookup(job_id)
        .map_err(|error| NativeJobError::new("store-error", error))?
        .ok_or_else(|| NativeJobError::new("invalid-input", "Official asset publication job is missing"))?;
    let shared = job
        .status
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .official_asset_publication_shared
        .as_ref()
        .map(|handle| Arc::clone(&handle.0))
        .ok_or_else(|| NativeJobError::new("invalid-input", "Native job is not an official asset publication"))?;
    let settled = settled(&shared);
    let entries = settled.iter().skip(offset).take(limit).cloned().collect();
    Ok(PublicationPage { entries, total: settled.len() })
}

#[tauri::command(async)]
pub(crate) fn native_official_asset_publication_settled(
    state: State<'_, NativeFileJobState>,
    job_id: String,
    offset: usize,
    limit: usize,
) -> Result<PublicationPage, NativeJobError> {
    logged("native_official_asset_publication_settled", settled_page(&state, &job_id, offset, limit))
}

pub(crate) fn publish(request: PublicationRequest, job: &JobControl) -> Result<JobResultSummary, NativeJobError> {
    let PublicationRequest { base_url, credential, account_id, session, concurrency, root, uploads, shared } = request;
    job.start(JobPhase::WritingExport).map_err(job_error)?;
    let writer = Writer::new(&base_url, &credential)?;
    let mut fingerprint = Sha256::new();
    for (key, _) in &uploads {
        fingerprint.update(key.as_bytes());
        fingerprint.update([0]);
    }
    // Writes share the overload state of the account's reads; a rejected
    // credential ends the job, and the renderer bounds how often it signs in again.
    let pause = scheduler::shared_pause(&account_id, base_url.trim_end_matches('/'));
    let mut scheduler = Scheduler::new(concurrency, pause, uploads.len(), false, Box::new(jitter));
    scheduler.receive(uploads.len());
    let mut run = Run {
        job,
        shared: &shared.0,
        scheduler,
        total: uploads.len(),
        in_flight: HashMap::new(),
        sequence: 0,
        failed: false,
        sent: Arc::default(),
        progress: PublicationProgress::default(),
        published: None,
    };
    let session = match session.filter(|session| !session.is_empty()) {
        Some(session) => Some(session),
        None => match tauri::async_runtime::block_on(writer.session(job))? {
            Session::Number(session) => Some(session),
            Session::Refused { reason, warning } => {
                run.warn(warning);
                run.scheduler.stop(reason);
                None
            }
        },
    };
    if let Some(session) = session {
        run.progress.session = Some(session.clone());
        tauri::async_runtime::block_on(run.drive(&writer, &uploads, &root, &session))?;
    }
    run.publish(true);
    Ok(JobResultSummary {
        export_exclusions: None,
        revision: 0,
        source_bytes: run.sent.load(Ordering::Relaxed),
        source_sha256: hex::encode(fingerprint.finalize()),
        source_fingerprint_kind: SourceFingerprintKind::WholeFileSha256,
        character_count: 0,
        preset_count: 0,
        warning_codes: Vec::new(),
        handoff_path: None,
        publication: None,
    })
}

struct Run<'a> {
    job: &'a JobControl,
    shared: &'a Mutex<Vec<SettledUpload>>,
    scheduler: Scheduler,
    total: usize,
    in_flight: HashMap<u64, Attempt>,
    sequence: u64,
    /// A write failed, so the publication fails and no other write starts.
    failed: bool,
    sent: Arc<AtomicU64>,
    progress: PublicationProgress,
    published: Option<Instant>,
}

impl Run<'_> {
    async fn drive(
        &mut self,
        writer: &Writer,
        uploads: &[(String, Option<String>)],
        root: &Path,
        session: &str,
    ) -> Result<(), NativeJobError> {
        let job = self.job;
        let mut writes = FuturesUnordered::new();
        loop {
            let cancelled = job.is_cancel_requested();
            if (cancelled || self.scheduler.reason().is_some()) && !self.in_flight.is_empty() {
                // Dropping a write aborts its request.
                writes = FuturesUnordered::new();
                for (_, attempt) in self.in_flight.drain() {
                    self.scheduler.abandon(attempt);
                }
            }
            if cancelled {
                return Err(NativeJobError::new("cancelled", "Official asset publication was cancelled"));
            }
            let now = now_ms();
            while let Some(attempt) = self.scheduler.next(now, !self.failed) {
                let (key, object) = &uploads[attempt.index];
                self.sequence += 1;
                let sequence = self.sequence;
                self.in_flight.insert(sequence, attempt);
                let sent = Arc::clone(&self.sent);
                let (root, object) = (root.to_path_buf(), object.clone());
                writes.push(async move {
                    // An evicted body downloads through the blocking sync client, so it opens off this runtime.
                    let body = tokio::task::spawn_blocking(move || object.and_then(|object| open_body(&root, &object)))
                        .await
                        .ok()
                        .flatten();
                    let write = match body {
                        Some((file, size)) => writer.write(key, file, size, session, job, sent).await,
                        // A pinned body that cannot be read fails the publication, as a missing payload does today.
                        None => ended(Exchange::Local),
                    };
                    (sequence, write)
                });
            }
            self.publish(false);
            if self.in_flight.is_empty() && (self.failed || self.scheduler.done()) {
                return Ok(());
            }
            let wait = self
                .scheduler
                .wake_at(!self.failed)
                .map_or(TICK, |at| TICK.min(Duration::from_millis(at.saturating_sub(now_ms()))));
            tokio::select! {
                Some((sequence, write)) = writes.next(), if !writes.is_empty() => self.finished(sequence, write),
                _ = tokio::time::sleep(wait) => {}
            }
        }
    }

    fn finished(&mut self, sequence: u64, write: Write) {
        let Some(attempt) = self.in_flight.remove(&sequence) else { return };
        let index = attempt.index;
        let (exchange, stored) = match write {
            Write::Cancelled => {
                self.scheduler.abandon(attempt);
                return;
            }
            Write::Warned { warning } => {
                self.scheduler.abandon(attempt);
                self.warn(warning);
                self.scheduler.stop(StopReason::AuthWarning);
                return;
            }
            Write::Stored { replacement_key, warning, reload_session } => {
                self.warn(warning);
                self.progress.reload_session |= reload_session;
                (Exchange::Body, Some(replacement_key))
            }
            Write::Ended { exchange, warning } => {
                self.warn(warning);
                (exchange, None)
            }
        };
        match (self.scheduler.finish(attempt, exchange, now_ms()), stored) {
            (Outcome::Install, Some(replacement_key)) => self.settle(index, Some(replacement_key)),
            (Outcome::Settled(_), _) => self.settle(index, None),
            _ => {}
        }
    }

    fn warn(&mut self, warning: Option<String>) {
        if warning.is_some() {
            self.progress.warning = warning;
        }
    }

    fn settle(&mut self, index: usize, replacement_key: Option<String>) {
        if replacement_key.is_some() {
            self.progress.written += 1;
        } else {
            self.progress.failed += 1;
            self.failed = true;
        }
        settled(self.shared).push(SettledUpload { index, replacement_key });
    }

    fn publish(&mut self, force: bool) {
        let now = Instant::now();
        if !force && self.published.is_some_and(|published| now.duration_since(published) < TICK) {
            return;
        }
        self.published = Some(now);
        self.progress.bytes = self.sent.load(Ordering::Relaxed);
        self.progress.active = self.scheduler.active();
        self.progress.limit = self.scheduler.limit();
        self.progress.waiting_until = self.scheduler.waiting_until(now_ms());
        self.progress.reason = self.scheduler.reason();
        let mut status = self.job.status.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if status.state != JobState::Running {
            return;
        }
        status.progress = JobProgress {
            completed_bytes: self.progress.bytes,
            total_bytes: None,
            completed_items: self.progress.written + self.progress.failed,
            total_items: Some(self.total as u64),
        };
        status.official_asset_publication = Some(self.progress.clone());
    }
}

fn open_body(root: &Path, object: &str) -> Option<(std::fs::File, u64)> {
    let file = crate::server_sync::residency::open_or_hydrate(root, object).ok()??;
    let size = file.metadata().ok()?.len();
    Some((file, size))
}

enum Session {
    Number(String),
    Refused { reason: StopReason, warning: Option<String> },
}

/// How one account write ended.
enum Write {
    Stored { replacement_key: String, warning: Option<String>, reload_session: bool },
    Ended { exchange: Exchange, warning: Option<String> },
    /// A 403 that warns instead of asking for a new sign-in.
    Warned { warning: Option<String> },
    Cancelled,
}

fn ended(exchange: Exchange) -> Write {
    Write::Ended { exchange, warning: None }
}

fn retryable() -> Write {
    ended(Exchange::Retryable { status: None, retry_after: None })
}

struct Writer {
    client: reqwest::Client,
    base_url: String,
    credential: HeaderMap,
}

impl Writer {
    fn new(base_url: &str, credential: &OfficialSnapshotCredential) -> Result<Self, NativeJobError> {
        validate_base_url(base_url)?;
        let client = crate::platform_tls::client_builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(30))
            .build()
            .map_err(|error| NativeJobError::new("network-error", error.to_string()))?;
        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_owned(),
            credential: authenticated_headers(credential)?,
        })
    }

    async fn session(&self, job: &JobControl) -> Result<Session, NativeJobError> {
        let response = await_network_operation(
            self.client
                .get(format!("{}/api/account/getsessionnumber", self.base_url))
                .headers(self.credential.clone())
                .send_logged("account-asset-write"),
            job,
            NETWORK_IDLE_TIMEOUT,
            "Official asset publication was cancelled",
        )
        .await?;
        let status = response.status().as_u16();
        if status == 403 {
            let warned = is_warning(&response);
            let warning = self.warning(response, job).await;
            let reason = if warned { StopReason::AuthWarning } else { StopReason::ReauthenticationNeeded };
            return Ok(Session::Refused { reason, warning });
        }
        let bytes = bounded_response_bytes(response, job, NETWORK_IDLE_TIMEOUT).await?;
        if !(200..300).contains(&status) {
            return Err(NativeJobError::new("network-error", format!("Official account session request returned {status}")));
        }
        let invalid = || NativeJobError::new("invalid-response", "Official account session response is invalid");
        let value = serde_json::from_slice::<Value>(&bytes).map_err(|_| invalid())?;
        let session = match &value["sessionNumber"] {
            Value::String(session) => session.clone(),
            Value::Number(session) => session.to_string(),
            _ => return Err(invalid()),
        };
        if session.is_empty() || session.len() > MAX_HEADER_BYTES || HeaderValue::from_str(&session).is_err() {
            return Err(invalid());
        }
        Ok(Session::Number(session))
    }

    async fn write(
        &self,
        key: &str,
        file: std::fs::File,
        size: u64,
        session: &str,
        job: &JobControl,
        sent: Arc<AtomicU64>,
    ) -> Write {
        // Header values are bytes; fetch sends a key's code points as Latin-1 and refuses anything wider.
        let Some(key_header) = latin1_header(key) else {
            return ended(Exchange::Permanent);
        };
        let progress = Arc::new(AtomicU64::new(0));
        let body = Counted { file: tokio::fs::File::from_std(file), sent: [Arc::clone(&progress), sent] };
        let request = self
            .client
            .post(format!("{}/api/account/write", self.base_url))
            .headers(self.credential.clone())
            .header(CONTENT_TYPE, "application/octet-stream")
            .header("x-risu-key", key_header)
            .header("x-format", "nocheck")
            .header("x-risu-session", session)
            .header("x-risu-save-date", now_ms().to_string())
            .header(CONTENT_LENGTH, size)
            .body(reqwest::Body::wrap_stream(ReaderStream::new(body)))
            .send_logged("account-asset-write");
        let response = match sending(request, job, &progress).await {
            Ok(response) => response,
            Err(write) => return write,
        };
        let status = response.status().as_u16();
        match status {
            304 => Write::Stored { replacement_key: key.to_owned(), warning: None, reload_session: false },
            // Only a 403 asks for a new sign-in; any other refusal fails the write, as the renderer's write does.
            403 => {
                let warned = is_warning(&response);
                let warning = self.warning(response, job).await;
                if warned {
                    Write::Warned { warning }
                } else {
                    Write::Ended { exchange: Exchange::Rejected, warning }
                }
            }
            408 | 429 | 500..=u16::MAX => ended(Exchange::Retryable {
                status: Some(status),
                retry_after: response.headers().get(RETRY_AFTER).and_then(|value| value.to_str().ok()).map(str::to_owned),
            }),
            200..=299 => self.stored(response, job).await,
            _ => ended(Exchange::Permanent),
        }
    }

    /// The account names the stored object in the response text, as the renderer's write reads it.
    async fn stored(&self, response: reqwest::Response, job: &JobControl) -> Write {
        let json = is_json(&response);
        let bytes = match bounded_response_bytes(response, job, NETWORK_IDLE_TIMEOUT).await {
            Ok(bytes) => bytes,
            Err(error) if error.code == "cancelled" => return Write::Cancelled,
            Err(error) if error.code == "network-error" => return retryable(),
            Err(_) => return ended(Exchange::Permanent),
        };
        let Ok(text) = String::from_utf8(bytes) else {
            return ended(Exchange::Permanent);
        };
        let (warning, reload_session) = if json {
            let Ok(value) = serde_json::from_str::<Value>(&text) else {
                return ended(Exchange::Permanent);
            };
            (
                value.get("warning").and_then(Value::as_str).map(str::to_owned),
                value.get("reloadSession").and_then(Value::as_bool).unwrap_or(false),
            )
        } else {
            (None, false)
        };
        if text.is_empty() || text.len() > MAX_KEY_BYTES {
            return Write::Ended { exchange: Exchange::Permanent, warning };
        }
        Write::Stored { replacement_key: text, warning, reload_session }
    }

    /// A 403 may carry a warning; a malformed one does not change the outcome.
    async fn warning(&self, response: reqwest::Response, job: &JobControl) -> Option<String> {
        if !is_json(&response) {
            return None;
        }
        let bytes = bounded_response_bytes(response, job, NETWORK_IDLE_TIMEOUT).await.ok()?;
        let value = serde_json::from_slice::<Value>(&bytes).ok()?;
        value.get("warning").and_then(Value::as_str).filter(|warning| warning.len() <= MAX_HEADER_BYTES).map(str::to_owned)
    }
}

fn is_warning(response: &reqwest::Response) -> bool {
    response.headers().get("x-risu-status").is_some_and(|value| value.as_bytes() == b"warn")
}

fn is_json(response: &reqwest::Response) -> bool {
    response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.split(';').next().unwrap_or_default().trim().eq_ignore_ascii_case("application/json"))
}

/// Waits for the response while the body is sent; every byte sent restarts the
/// idle deadline, so it bounds the wait for headers after the last byte.
async fn sending<F>(request: F, job: &JobControl, progress: &AtomicU64) -> Result<reqwest::Response, Write>
where
    F: Future<Output = Result<reqwest::Response, reqwest::Error>>,
{
    let mut request = std::pin::pin!(request);
    let mut seen = progress.load(Ordering::Relaxed);
    let mut idle_since = Instant::now();
    loop {
        if job.is_cancel_requested() {
            return Err(Write::Cancelled);
        }
        if let Ok(result) = tokio::time::timeout(TICK, request.as_mut()).await {
            if job.is_cancel_requested() {
                return Err(Write::Cancelled);
            }
            return result.map_err(|_| retryable());
        }
        let current = progress.load(Ordering::Relaxed);
        if current != seen {
            seen = current;
            idle_since = Instant::now();
        } else if idle_since.elapsed() >= NETWORK_IDLE_TIMEOUT {
            return Err(retryable());
        }
    }
}

/// The body of one write, counting the bytes handed to the connection.
struct Counted {
    file: tokio::fs::File,
    sent: [Arc<AtomicU64>; 2],
}

impl AsyncRead for Counted {
    fn poll_read(mut self: Pin<&mut Self>, context: &mut Context<'_>, buffer: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        let before = buffer.filled().len();
        let poll = Pin::new(&mut self.file).poll_read(context, buffer);
        let read = (buffer.filled().len() - before) as u64;
        for counter in &self.sent {
            counter.fetch_add(read, Ordering::Relaxed);
        }
        poll
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asset_repository::PayloadCas;
    use crate::native_file_jobs::{JobKind, JobRegistry};
    use crate::persistent_store::AssetAlias;
    use std::io::{Read as _, Write as _};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::AtomicBool;
    use std::thread;

    type Reply = dyn Fn(&str, &str, usize) -> Vec<u8> + Send + Sync;

    struct Seen {
        path: String,
        key: String,
        head: String,
        body: Vec<u8>,
    }

    /// A loopback account that answers each request by path, key and attempt number.
    struct Account {
        url: String,
        stop: Arc<AtomicBool>,
        seen: Arc<Mutex<Vec<Seen>>>,
    }

    impl Account {
        fn start(reply: impl Fn(&str, &str, usize) -> Vec<u8> + Send + Sync + 'static) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let stop = Arc::new(AtomicBool::new(false));
            let seen = Arc::new(Mutex::new(Vec::new()));
            let reply: Arc<Reply> = Arc::new(reply);
            let (stopped, recorded) = (Arc::clone(&stop), Arc::clone(&seen));
            thread::spawn(move || {
                while !stopped.load(Ordering::Acquire) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            let (recorded, reply) = (Arc::clone(&recorded), Arc::clone(&reply));
                            thread::spawn(move || answer(stream, &recorded, reply.as_ref()));
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => thread::sleep(Duration::from_millis(2)),
                        Err(error) => panic!("fixture accept failed: {error}"),
                    }
                }
            });
            Self { url, stop, seen }
        }

        fn requests(&self, path: &str) -> Vec<(String, String, Vec<u8>)> {
            self.seen.lock().unwrap().iter().filter(|seen| seen.path == path)
                .map(|seen| (seen.key.clone(), seen.head.clone(), seen.body.clone())).collect()
        }
    }

    impl Drop for Account {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
        }
    }

    fn answer(mut stream: TcpStream, seen: &Mutex<Vec<Seen>>, reply: &Reply) {
        stream.set_nonblocking(false).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut request = Vec::new();
        let mut buffer = [0u8; 64 * 1024];
        let end = loop {
            if let Some(end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                break end + 4;
            }
            match stream.read(&mut buffer) {
                Ok(0) | Err(_) => return,
                Ok(read) => request.extend_from_slice(&buffer[..read]),
            }
        };
        let head = String::from_utf8(request[..end].to_vec()).unwrap();
        let header = |name: &str| head.lines()
            .find_map(|line| line.split_once(':').filter(|(found, _)| found.eq_ignore_ascii_case(name)).map(|(_, value)| value.trim().to_owned()));
        let length = header("content-length").map_or(0, |value| value.parse::<usize>().unwrap());
        let mut body = request[end..].to_vec();
        while body.len() < length {
            match stream.read(&mut buffer) {
                Ok(0) | Err(_) => return,
                Ok(read) => body.extend_from_slice(&buffer[..read]),
            }
        }
        let path = head.split(' ').nth(1).unwrap().to_owned();
        let key = header("x-risu-key").unwrap_or_default();
        let attempt = {
            let mut seen = seen.lock().unwrap();
            seen.push(Seen { path: path.clone(), key: key.clone(), head, body });
            seen.iter().filter(|seen| seen.path == path && seen.key == key).count()
        };
        let _ = stream.write_all(&reply(&path, &key, attempt));
    }

    fn response(status: &str, headers: &[(&str, &str)], body: &[u8]) -> Vec<u8> {
        let mut head = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n", body.len());
        for (name, value) in headers {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        head.push_str("\r\n");
        let mut bytes = head.into_bytes();
        bytes.extend_from_slice(body);
        bytes
    }

    const JSON: (&str, &str) = ("Content-Type", "application/json");
    const WRITE: &str = "/api/account/write";
    const SESSION: &str = "/api/account/getsessionnumber";

    /// A library whose aliases name the given bodies.
    fn library(bodies: &[(&str, &[u8])]) -> (tempfile::TempDir, PersistentStore) {
        let directory = tempfile::tempdir().unwrap();
        let mut store = PersistentStore::open(directory.path()).unwrap();
        let cas = PayloadCas::new(directory.path()).unwrap();
        for (key, body) in bodies {
            let alias = AssetAlias {
                key: (*key).to_owned(),
                object_hash: Some(cas.prepare_bytes(body).unwrap().content_hash),
                kind: "asset".to_owned(),
                size: body.len() as i64,
                mime: String::new(),
                name: "synthetic".to_owned(),
                ext: "png".to_owned(),
                inlay_type: None,
                width: None,
                height: None,
                metadata: serde_json::json!({}),
            };
            store.commit_asset_alias(&alias, store.revision().unwrap()).unwrap();
        }
        (directory, store)
    }

    struct Published {
        outcome: Result<JobResultSummary, NativeJobError>,
        progress: PublicationProgress,
        settled: Vec<(String, Option<String>)>,
    }

    fn publish_uploads(url: &str, uploads: Vec<(String, Option<String>)>, root: PathBuf, session: Option<&str>, concurrency: usize) -> Published {
        let keys = uploads.iter().map(|(key, _)| key.clone()).collect::<Vec<_>>();
        let shared = PublicationHandle::default();
        let job = JobRegistry::default().create(JobKind::PublishOfficialAccountAssets).unwrap();
        let outcome = publish(PublicationRequest {
            base_url: url.to_owned(),
            credential: OfficialSnapshotCredential::RisuAuth { token: "synthetic-token".to_owned() },
            account_id: uuid::Uuid::new_v4().to_string(),
            session: session.map(str::to_owned),
            concurrency,
            root,
            uploads,
            shared: shared.clone(),
        }, &job);
        let settled = settled(&shared.0).iter().map(|entry| (keys[entry.index].clone(), entry.replacement_key.clone())).collect();
        Published { outcome, progress: job.status().official_asset_publication.unwrap(), settled }
    }

    /// Publishes the given aliases as they stand in a leased revision.
    fn publish_library(store: &mut PersistentStore, url: &str, keys: &[&str], session: Option<&str>, concurrency: usize) -> Published {
        let lease = store.acquire_revision(store.revision().unwrap()).unwrap().lease;
        let assets = keys.iter().map(|key| PinnedAsset { local_key: (*key).to_owned(), key: (*key).to_owned() }).collect::<Vec<_>>();
        // The lease keeps the published revision readable after the library moves on.
        for key in keys {
            store.delete_asset_alias("asset", key, store.revision().unwrap()).unwrap();
        }
        let (root, uploads) = pinned_uploads(store, &lease, &assets).unwrap();
        publish_uploads(url, uploads, root, session, concurrency)
    }

    #[test]
    fn bodies_stream_into_writes_and_settle_with_the_key_the_account_returns() {
        let bodies: [(&str, &[u8]); 4] = [
            ("assets/a.png", b"first synthetic body"),
            ("assets/b.png", b"second"),
            ("assets/c.png", b"third synthetic body"),
            ("assets/d.png", b"fourth"),
        ];
        let (_directory, mut store) = library(&bodies);
        let account = Account::start(|path, key, attempt| match (path, key, attempt) {
            (SESSION, _, _) => response("200 OK", &[JSON], br#"{"sessionNumber":7}"#),
            (_, "assets/a.png", _) => response("200 OK", &[], b"assets/a-stored.png"),
            (_, "assets/b.png", _) => response("304 Not Modified", &[], b""),
            (_, "assets/c.png", 1) => response("503 Service Unavailable", &[("Retry-After", "0")], b""),
            (_, "assets/c.png", _) => response("200 OK", &[], b"assets/c-stored.png"),
            _ => response("200 OK", &[JSON], br#"{"warning":"synthetic-warning","reloadSession":true}"#),
        });

        let published = publish_library(&mut store, &account.url, &bodies.map(|(key, _)| key), None, 2);

        published.outcome.unwrap();
        let mut settled = published.settled;
        settled.sort();
        assert_eq!(settled, [
            ("assets/a.png".to_owned(), Some("assets/a-stored.png".to_owned())),
            ("assets/b.png".to_owned(), Some("assets/b.png".to_owned())),
            ("assets/c.png".to_owned(), Some("assets/c-stored.png".to_owned())),
            // The renderer's write takes the whole response text as the key.
            ("assets/d.png".to_owned(), Some(r#"{"warning":"synthetic-warning","reloadSession":true}"#.to_owned())),
        ]);
        let progress = published.progress;
        assert_eq!((progress.written, progress.failed, progress.reason), (4, 0, None));
        assert_eq!(progress.session.as_deref(), Some("7"));
        assert_eq!(progress.warning.as_deref(), Some("synthetic-warning"));
        assert!(progress.reload_session);
        assert_eq!(account.requests(SESSION).len(), 1);
        let writes = account.requests(WRITE);
        assert_eq!(writes.len(), 5);
        for (key, body) in bodies {
            let (_, head, sent) = writes.iter().rev().find(|(written, _, _)| written == key).unwrap();
            assert_eq!(sent, body, "{key}");
            for header in [
                "x-risu-auth: synthetic-token\r\n".to_owned(),
                "content-type: application/octet-stream\r\n".to_owned(),
                "x-format: nocheck\r\n".to_owned(),
                "x-risu-session: 7\r\n".to_owned(),
                format!("content-length: {}\r\n", body.len()),
            ] {
                assert!(head.to_ascii_lowercase().contains(&header), "{key} lacks {header:?} in {head}");
            }
            assert!(head.to_ascii_lowercase().contains("x-risu-save-date: "), "{key}");
        }
    }

    #[test]
    fn a_failed_write_fails_the_publication_and_starts_no_other_write() {
        for status in ["404 Not Found", "401 Unauthorized"] {
            let (_directory, mut store) = library(&[("assets/a.png", b"a"), ("assets/b.png", b"b"), ("assets/c.png", b"c")]);
            let account = Account::start(move |_, _, _| response(status, &[], b""));

            let published = publish_library(&mut store, &account.url, &["assets/a.png", "assets/b.png", "assets/c.png"], Some("9"), 1);

            published.outcome.unwrap();
            assert_eq!(published.settled, [("assets/a.png".to_owned(), None)], "{status}");
            assert_eq!((published.progress.written, published.progress.failed), (0, 1), "{status}");
            assert_eq!(published.progress.reason, None, "{status}");
            assert_eq!(account.requests(WRITE).len(), 1, "{status}");
            assert!(account.requests(SESSION).is_empty(), "{status}");
        }
    }

    #[test]
    fn rejections_end_the_run_without_settling_the_asset() {
        let warning = br#"{"warning":"synthetic-warning"}"#;
        for (path, status, warned, session, reason, expected_warning) in [
            (WRITE, "403 Forbidden", false, Some("9"), StopReason::ReauthenticationNeeded, Some("synthetic-warning")),
            (WRITE, "403 Forbidden", true, Some("9"), StopReason::AuthWarning, Some("synthetic-warning")),
            (SESSION, "403 Forbidden", false, None, StopReason::ReauthenticationNeeded, Some("synthetic-warning")),
            (SESSION, "403 Forbidden", true, None, StopReason::AuthWarning, Some("synthetic-warning")),
        ] {
            let (_directory, mut store) = library(&[("assets/a.png", b"a")]);
            let account = Account::start(move |requested, _, _| {
                let headers: &[(&str, &str)] = if warned { &[JSON, ("x-risu-status", "warn")] } else { &[JSON] };
                if requested == path { response(status, headers, warning) } else { response("200 OK", &[JSON], br#"{"sessionNumber":7}"#) }
            });

            let published = publish_library(&mut store, &account.url, &["assets/a.png"], session, 2);

            published.outcome.unwrap();
            assert!(published.settled.is_empty(), "{path} {status}");
            assert_eq!(published.progress.reason, Some(reason), "{path} {status}");
            assert_eq!(published.progress.warning.as_deref(), expected_warning, "{path} {status}");
            assert_eq!(account.requests(WRITE).len(), usize::from(path == WRITE), "{path} {status}");
        }
    }

    #[test]
    fn a_body_the_library_cannot_open_stops_the_run_as_a_local_failure() {
        let directory = tempfile::tempdir().unwrap();
        let account = Account::start(|_, _, _| response("200 OK", &[], b"assets/stored.png"));
        let missing = "0".repeat(64);

        let published = publish_uploads(&account.url, vec![
            ("assets/a.png".to_owned(), None),
            ("assets/b.png".to_owned(), Some(missing)),
        ], directory.path().to_owned(), Some("9"), 1);

        published.outcome.unwrap();
        assert_eq!(published.settled, [("assets/a.png".to_owned(), None)]);
        assert_eq!(published.progress.reason, Some(StopReason::Local));
        assert!(account.requests(WRITE).is_empty());
    }

    #[test]
    fn start_requests_are_bounded_to_asset_keys() {
        let asset = |key: &str| PinnedAsset { local_key: key.to_owned(), key: key.to_owned() };
        let valid = [asset("assets/a.png"), asset("assets\\b.png")];
        assert_eq!(validate_publication_start("https://account.invalid", "account", "snapshot-1", Some("7"), &valid, None).unwrap(), 2);
        for (lease, session, assets, concurrency) in [
            ("lease-1", None, vec![asset("assets/a.png")], None),
            ("snapshot-1", Some("bad\nsession"), vec![asset("assets/a.png")], None),
            ("snapshot-1", None, vec![asset("database/database.bin")], None),
            ("snapshot-1", None, vec![PinnedAsset { local_key: "inlay/a".to_owned(), key: "assets/a.png".to_owned() }], None),
            ("snapshot-1", None, vec![asset("assets/a.png")], Some(3)),
        ] {
            let error = validate_publication_start("https://account.invalid", "account", lease, session, &assets, concurrency).unwrap_err();
            assert_eq!(error.code, "invalid-input");
        }
    }
}
