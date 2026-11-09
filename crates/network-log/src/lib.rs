mod export;
mod masking;
mod model;
#[cfg(any(test, feature = "reqwest"))]
pub mod reqwest;
mod storage;
pub use model::*;
pub use storage::Snapshot;

use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicU8, AtomicUsize, Ordering},
        mpsc::{self, SyncSender},
        Arc, Mutex, OnceLock, RwLock,
    },
    time::{Instant, SystemTime, UNIX_EPOCH},
};

const QUEUE_EVENTS: usize = 16_384;
const ACTIVE_LIMIT: usize = 64;
const CHUNK_BYTES: usize = 64 * 1024;
// A burst that fills the largest buffer runs out of bytes before the event queue fills.
const _: () = assert!(QUEUE_EVENTS * CHUNK_BYTES >= (BUFFER_MIB_CHOICES[BUFFER_MIB_CHOICES.len() - 1] * MIB) as usize);
static GLOBAL: OnceLock<RwLock<Option<Recorder>>> = OnceLock::new();

#[derive(Clone)]
pub struct Recorder(Arc<Shared>);
struct Shared {
    sender: SyncSender<Event>,
    settings: Mutex<Settings>,
    enabled: AtomicBool,
    queued: AtomicUsize,
    queue_limit: AtomicUsize,
    active: AtomicUsize,
    warning: Mutex<Option<String>>,
    stopping: AtomicBool,
    stopped: AtomicBool,
}
/// Why a whole capture, or one body side, stopped recording.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Stop {
    Stopped = 1,
    QueueFull,
    Quota,
    WriteFailed,
}
impl Stop {
    fn load(cell: &AtomicU8) -> Option<Self> {
        match cell.load(Ordering::Relaxed) {
            1 => Some(Self::Stopped),
            2 => Some(Self::QueueFull),
            3 => Some(Self::Quota),
            4 => Some(Self::WriteFailed),
            _ => None,
        }
    }
    fn from_error(error: &str) -> Self {
        if error == "quota" {
            Self::Quota
        } else {
            Self::WriteFailed
        }
    }
    fn as_str(self) -> &'static str {
        match self {
            Self::Stopped => "stopped",
            Self::QueueFull => "queue-full",
            Self::Quota => "quota",
            Self::WriteFailed => "write-failed",
        }
    }
}
/// Which active records the writer persists on a flush.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Flush {
    /// Records that changed and whose throttle interval has passed.
    Due,
    /// Every changed record, before a job reads or moves them.
    Changed,
    /// Every record, finishing each one, before the writer exits.
    Final,
}
struct Signal {
    id: String,
    owner: Arc<Shared>,
    start: Instant,
    hide: bool,
    limit: u64,
    binary_limit: u64,
    websocket: bool,
    pending: AtomicUsize,
    outcome: AtomicU8,
    reason: AtomicU8,
    body_reason: [AtomicU8; 2],
    response: Mutex<Option<Message>>,
    representations: Mutex<[Option<&'static str>; 2]>,
    dirty: AtomicBool,
    elapsed: AtomicU64,
    observed: [AtomicU64; 2],
    admitted: [AtomicU64; 2],
    binary_admitted: [AtomicU64; 2],
    binary: [AtomicBool; 2],
    partial: AtomicBool,
    metadata_limited: AtomicBool,
    limited: [AtomicBool; 2],
    skipped: [AtomicBool; 2],
}
impl Signal {
    fn stop(&self, reason: Stop) {
        self.record_stop(&self.reason, reason);
    }
    fn stop_body(&self, side: Side, reason: Stop) {
        self.record_stop(&self.body_reason[side.index()], reason);
    }
    fn record_stop(&self, cell: &AtomicU8, reason: Stop) {
        let _ = cell.compare_exchange(0, reason as u8, Ordering::AcqRel, Ordering::Relaxed);
        self.dirty.store(true, Ordering::Release);
        if reason == Stop::QueueFull {
            *self.owner.warning.lock().unwrap() = Some("queue-full".into());
        }
    }
    fn stopped(&self) -> Option<Stop> {
        Stop::load(&self.reason)
    }
    fn body_stopped(&self, side: Side) -> Option<Stop> {
        Stop::load(&self.body_reason[side.index()])
    }
    fn failing(&self) -> bool {
        [
            self.stopped(),
            self.body_stopped(Side::Request),
            self.body_stopped(Side::Response),
        ]
        .into_iter()
        .flatten()
        .any(|stop| stop != Stop::Stopped)
    }
    fn limit_metadata(&self) {
        self.metadata_limited.store(true, Ordering::Relaxed);
        self.dirty.store(true, Ordering::Release);
    }
    fn capture_error(&self) -> Option<&'static str> {
        let any = |flags: &[AtomicBool; 2]| flags.iter().any(|flag| flag.load(Ordering::Relaxed));
        if let Some(stop) = self
            .stopped()
            .or_else(|| self.body_stopped(Side::Request))
            .or_else(|| self.body_stopped(Side::Response))
        {
            Some(stop.as_str())
        } else if self.metadata_limited.load(Ordering::Relaxed) {
            Some("metadata-limit")
        } else if any(&self.limited) {
            Some("body-limit")
        } else if self.partial.load(Ordering::Relaxed) {
            Some("started-late")
        } else if any(&self.skipped) {
            Some("binary-skipped")
        } else {
            None
        }
    }
}
impl Drop for Signal {
    fn drop(&mut self) {
        self.owner.active.fetch_sub(1, Ordering::Relaxed);
    }
}
#[derive(Clone, Default)]
pub struct Capture(Option<Arc<Signal>>);
pub struct CaptureGuard(pub Capture);
impl Drop for CaptureGuard {
    fn drop(&mut self) {
        self.0.finish("incomplete");
    }
}
impl std::ops::Deref for CaptureGuard {
    type Target = Capture;
    fn deref(&self) -> &Capture {
        &self.0
    }
}

struct Envelope {
    signal: Arc<Signal>,
    bytes: usize,
    event: Payload,
}
impl Drop for Envelope {
    fn drop(&mut self) {
        self.signal
            .owner
            .queued
            .fetch_sub(self.bytes, Ordering::Relaxed);
        self.signal.pending.fetch_sub(1, Ordering::Release);
    }
}
enum Payload {
    Start(Box<Record>),
    Bytes(Side, Vec<u8>, u64, Option<String>),
}
enum Event {
    Wake,
    Capture(Envelope),
    Job(Box<dyn FnOnce(&mut storage::Store) + Send>),
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
fn binary(headers: &http::HeaderMap) -> bool {
    let content_type = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    !content_type.is_empty()
        && !content_type.starts_with("text/")
        && !["json", "xml", "javascript", "x-www-form-urlencoded"]
            .iter()
            .any(|s| content_type.contains(s))
}
/// Describes a body as the transport exposes it. A content coding the transport
/// did not remove is stored as received rather than decoded for display.
fn observed_body(headers: &http::HeaderMap) -> Body {
    let encoding = headers
        .get("content-encoding")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim().to_ascii_lowercase())
        .filter(|v| !v.is_empty() && v != "identity");
    Body {
        binary: binary(headers) || encoding.is_some(),
        representation: encoding.map_or_else(
            || "observed".into(),
            |e| format!("encoded-{}", e.chars().take(32).collect::<String>()),
        ),
        ..Body::default()
    }
}
pub fn install(recorder: Recorder) {
    *GLOBAL.get_or_init(Default::default).write().unwrap() = Some(recorder);
}
pub fn global() -> Option<Recorder> {
    GLOBAL.get().and_then(|g| g.read().unwrap().clone())
}
pub fn begin(source: &str, method: &str, url: &str, headers: &http::HeaderMap) -> CaptureGuard {
    CaptureGuard(
        global()
            .map(|r| r.begin(source, method, url, headers))
            .unwrap_or_default(),
    )
}
pub fn enabled() -> bool {
    global().is_some_and(|r| r.0.enabled.load(Ordering::Relaxed))
}

pub fn begin_operation(source: &str, method: &str, url: &str, headers: &http::HeaderMap, operation_id: uuid::Uuid) -> CaptureGuard {
    CaptureGuard(global().map(|r| r.begin_operation(source, method, url, headers, Some(operation_id))).unwrap_or_default())
}

impl Recorder {
    pub fn open(recent: PathBuf, retained: PathBuf) -> Result<Self, String> {
        let (sender, receiver) = mpsc::sync_channel(QUEUE_EVENTS);
        let mut store = storage::Store::open(recent, retained)?;
        let settings = store.settings()?;
        let shared = Arc::new(Shared {
            sender,
            enabled: AtomicBool::new(settings.enabled),
            queue_limit: AtomicUsize::new((settings.buffer_mib * MIB) as usize),
            settings: Mutex::new(settings),
            queued: AtomicUsize::new(0),
            active: AtomicUsize::new(0),
            warning: Mutex::new(None),
            stopping: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
        });
        let owner = shared.clone();
        std::thread::Builder::new()
            .name("network-log".into())
            .spawn(move || {
                let mut active = std::collections::HashMap::<String, (Arc<Signal>, Record)>::new();
                loop {
                    if owner.stopping.load(Ordering::Acquire) {
                        for (signal, _) in active.values() {
                            signal.stop(Stop::Stopped);
                        }
                        flush(&mut store, &mut active, Flush::Final);
                        let _ = store.clear_recent();
                        break;
                    }
                    let next = if active.is_empty() {
                        receiver
                            .recv()
                            .map_err(|_| mpsc::RecvTimeoutError::Disconnected)
                    } else {
                        receiver.recv_timeout(std::time::Duration::from_millis(50))
                    };
                    let event = match next {
                        Ok(event) => event,
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            flush(&mut store, &mut active, Flush::Due);
                            continue;
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    };
                    match event {
                        Event::Wake => {}
                        Event::Capture(envelope) => {
                            let signal = &envelope.signal;
                            let settings = owner.settings.lock().unwrap().clone();
                            let result = match &envelope.event {
                                Payload::Start(record) => {
                                    let result = store.insert(record, &settings);
                                    if result.is_ok() {
                                        active.insert(
                                            record.id.clone(),
                                            (signal.clone(), record.as_ref().clone()),
                                        );
                                    }
                                    result
                                }
                                Payload::Bytes(side, bytes, elapsed, kind) => {
                                    if let Some((_, record)) = active.get_mut(&signal.id) {
                                        apply_metadata(&mut store, signal, record, &settings);
                                        signal.dirty.store(true, Ordering::Release);
                                        // Bytes admitted before a disk failure on this side are dropped.
                                        if matches!(
                                            signal.body_stopped(*side),
                                            Some(Stop::Quota | Stop::WriteFailed)
                                        ) {
                                            Ok(())
                                        } else {
                                            store.append(
                                                record,
                                                *side,
                                                bytes,
                                                *elapsed,
                                                kind.as_deref(),
                                                &settings,
                                            )
                                        }
                                    } else {
                                        Ok(())
                                    }
                                }
                            };
                            if let Err(error) = result {
                                let reason = Stop::from_error(&error);
                                if let Payload::Bytes(side, ..) = &envelope.event {
                                    signal.stop_body(*side, reason);
                                } else {
                                    signal.stop(reason);
                                }
                                *owner.warning.lock().unwrap() = Some(error);
                            }
                        }
                        Event::Job(job) => {
                            flush(&mut store, &mut active, Flush::Changed);
                            job(&mut store);
                            // Promotion updates the writer's copy before accepting more chunks.
                            for (id, (_, record)) in &mut active {
                                if let Ok(saved) = store.record(id) {
                                    record.retained = saved.retained;
                                }
                            }
                        }
                    }
                    flush(&mut store, &mut active, Flush::Due);
                }
                drop(store);
                owner.stopped.store(true, Ordering::Release);
            })
            .map_err(|_| "network-log-unavailable".to_string())?;
        Ok(Self(shared))
    }
    pub fn settings(&self) -> Settings {
        self.0.settings.lock().unwrap().clone()
    }
    pub fn configure(&self, settings: Settings) -> Result<(), String> {
        settings.validate()?;
        let next = settings.clone();
        self.job(move |store| store.configure(&next))?;
        if !settings.enabled {
            self.0.enabled.store(false, Ordering::Release);
        }
        self.0.queue_limit.store((settings.buffer_mib * MIB) as usize, Ordering::Release);
        *self.0.settings.lock().unwrap() = settings.clone();
        self.0.enabled.store(settings.enabled, Ordering::Release);
        *self.0.warning.lock().unwrap() = None;
        if !settings.enabled {
            self.job(|store| {
                store.stop_active = true;
                Ok(())
            })?;
        }
        Ok(())
    }
    pub fn begin(
        &self,
        source: &str,
        method: &str,
        url: &str,
        headers: &http::HeaderMap,
    ) -> Capture {
        self.begin_operation(source, method, url, headers, None)
    }
    pub fn begin_operation(
        &self,
        source: &str,
        method: &str,
        url: &str,
        headers: &http::HeaderMap,
        operation_id: Option<uuid::Uuid>,
    ) -> Capture {
        if !self.0.enabled.load(Ordering::Relaxed) {
            return Capture::default();
        }
        if self
            .0
            .active
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| {
                (n < ACTIVE_LIMIT).then_some(n + 1)
            })
            .is_err()
        {
            *self.0.warning.lock().unwrap() = Some("concurrent-limit".into());
            return Capture::default();
        }
        let settings = self.settings();
        let request_body = observed_body(headers);
        let signal = Arc::new(Signal {
            id: uuid::Uuid::new_v4().to_string(),
            owner: self.0.clone(),
            start: Instant::now(),
            hide: settings.hide_auth,
            limit: BODY_BYTES,
            binary_limit: if settings.capture_binary { settings.buffer_mib * MIB } else { 0 },
            websocket: method == "WS",
            pending: AtomicUsize::new(0),
            outcome: AtomicU8::new(0),
            reason: AtomicU8::new(0),
            body_reason: [AtomicU8::new(0), AtomicU8::new(0)],
            response: Mutex::new(None),
            representations: Mutex::new([None, None]),
            dirty: AtomicBool::new(true),
            elapsed: AtomicU64::new(0),
            observed: [AtomicU64::new(0), AtomicU64::new(0)],
            admitted: [AtomicU64::new(0), AtomicU64::new(0)],
            binary_admitted: [AtomicU64::new(0), AtomicU64::new(0)],
            binary: [AtomicBool::new(request_body.binary), AtomicBool::new(false)],
            partial: AtomicBool::new(false),
            metadata_limited: AtomicBool::new(false),
            limited: [AtomicBool::new(false), AtomicBool::new(false)],
            skipped: [AtomicBool::new(false), AtomicBool::new(false)],
        });
        let capture = Capture(Some(signal.clone()));
        let headers = match masking::headers(headers, settings.hide_auth) {
            Ok(h) => h,
            Err(_) => {
                signal.limit_metadata();
                vec![]
            }
        };
        if url.len() > 8192 || source.len() > 80 || method.len() > 32 {
            signal.limit_metadata();
        }
        let shorten = |s: &str, n| s.chars().take(n).collect::<String>();
        let record = Record {
            id: signal.id.clone(),
            operation_id: operation_id.map(|id| id.to_string()),
            source: shorten(source, 80),
            started_at: now(),
            elapsed_ms: 0,
            retained: settings.retain,
            header_masking: settings.hide_auth,
            state: "active".into(),
            capture_error: None,
            request: Message {
                method: Some(shorten(method, 32)),
                url: shorten(url, 8192),
                headers,
                body: request_body,
                status: None,
            },
            response: Message::default(),
            stream_kind: (method == "WS").then(|| "websocket".into()),
            event_bytes: 0,
        };
        let bytes = record.request.queued_bytes() + record.source.capacity() + 2048;
        capture.enqueue(Payload::Start(Box::new(record)), bytes);
        capture
    }
    fn job<T: Send + 'static>(
        &self,
        job: impl FnOnce(&mut storage::Store) -> Result<T, String> + Send + 'static,
    ) -> Result<T, String> {
        if self.0.stopping.load(Ordering::Acquire) {
            return Err("network-log-unavailable".into());
        }
        let (tx, rx) = mpsc::channel();
        self.0
            .sender
            .send(Event::Job(Box::new(move |store| {
                let _ = tx.send(job(store));
            })))
            .map_err(|_| "network-log-unavailable".to_string())?;
        rx.recv()
            .map_err(|_| "network-log-unavailable".to_string())?
    }
    pub fn list(&self, query: Query) -> Result<Listing, String> {
        let mut list = self.job(move |store| store.list(&query))?;
        list.warning = self.0.warning.lock().unwrap().clone().or(list.warning);
        Ok(list)
    }
    pub fn record(&self, id: String) -> Result<Record, String> {
        self.job(move |s| s.record(&id))
    }
    pub fn snapshot(&self, id: String) -> Result<Snapshot, String> {
        self.job(move |s| s.snapshot(&id))
    }
    pub fn retain(&self, id: String) -> Result<(), String> {
        let settings = self.settings();
        self.job(move |s| s.retain(&id, &settings))
    }
    pub fn delete(&self, id: String) -> Result<(), String> {
        self.job(move |s| s.delete(&id))?;
        *self.0.warning.lock().unwrap() = None;
        Ok(())
    }
    pub fn shutdown(&self) {
        self.0.enabled.store(false, Ordering::Release);
        self.0.stopping.store(true, Ordering::Release);
        let _ = self.0.sender.try_send(Event::Wake);
        let deadline = Instant::now() + std::time::Duration::from_secs(3);
        while !self.0.stopped.load(Ordering::Acquire) && Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
    pub fn is_stopped(&self) -> bool {
        self.0.stopped.load(Ordering::Acquire)
    }
}
fn apply_metadata(
    store: &mut storage::Store,
    signal: &Signal,
    record: &mut Record,
    settings: &Settings,
) {
    if let Some(response) = signal.response.lock().unwrap().take() {
        record.response = response;
        if record.response.headers.iter().any(|h| {
            h.name.eq_ignore_ascii_case("content-type") && h.value.contains("text/event-stream")
        }) {
            record.stream_kind = Some("sse".into());
        }
        if let Err(error) = store.admit_metadata(record, settings) {
            record.response.headers.clear();
            signal.stop_body(Side::Response, Stop::from_error(&error));
            *signal.owner.warning.lock().unwrap() = Some(error);
        }
        signal.dirty.store(true, Ordering::Release);
    }
    for (index, value) in signal
        .representations
        .lock()
        .unwrap()
        .iter_mut()
        .enumerate()
    {
        if let Some(value) = value.take() {
            record
                .body_mut(if index == 0 {
                    Side::Request
                } else {
                    Side::Response
                })
                .representation = value.into();
            signal.dirty.store(true, Ordering::Release);
        }
    }
}
fn flush(
    store: &mut storage::Store,
    active: &mut std::collections::HashMap<String, (Arc<Signal>, Record)>,
    mode: Flush,
) {
    let stop = std::mem::take(&mut store.stop_active);
    let mut recovered = None;
    active.retain(|_, (signal, record)| {
        if stop {
            signal.stop(Stop::Stopped);
        }
        let outcome = signal.outcome.load(Ordering::Acquire);
        let terminal = (outcome != 0 || signal.stopped() == Some(Stop::Stopped))
            && signal.pending.load(Ordering::Acquire) == 0;
        let finishing = terminal || mode == Flush::Final;
        if mode == Flush::Due
            && !terminal
            && (signal.start.elapsed().as_millis() as u64) < record.elapsed_ms + 250
        {
            return true;
        }
        let settings = signal.owner.settings.lock().unwrap().clone();
        apply_metadata(store, signal, record, &settings);
        if !finishing && !signal.dirty.swap(false, Ordering::AcqRel) {
            return true;
        }
        record.elapsed_ms = if outcome == 0 {
            signal.start.elapsed().as_millis() as u64
        } else {
            signal.elapsed.load(Ordering::Relaxed).saturating_sub(1)
        };
        for side in [Side::Request, Side::Response] {
            let body = record.body_mut(side);
            body.observed_bytes = signal.observed[side.index()].load(Ordering::Relaxed);
            body.truncated = body.captured_bytes < body.observed_bytes;
            body.binary = signal.binary[side.index()].load(Ordering::Relaxed);
        }
        if finishing {
            record.state = match outcome {
                1 => "complete",
                2 => "error",
                _ => "incomplete",
            }
            .into();
        }
        record.capture_error = signal.capture_error().map(str::to_owned);
        if let Err(error) = store.update(record) {
            signal.stop(Stop::WriteFailed);
            *signal.owner.warning.lock().unwrap() = Some(error);
        } else if terminal && outcome != 0 && !signal.failing() {
            recovered = Some(signal.owner.clone());
        }
        if finishing {
            store.finish(&record.id);
        }
        !finishing
    });
    // A capture that finished without a recording failure clears the recorder warning,
    // unless another capture is still failing.
    if let Some(owner) = recovered {
        if !active.values().any(|(signal, _)| signal.failing()) {
            *owner.warning.lock().unwrap() = None;
        }
    }
}
impl Capture {
    pub fn is_active(&self) -> bool {
        self.0
            .as_ref()
            .is_some_and(|s| s.stopped().is_none() && s.owner.enabled.load(Ordering::Relaxed))
    }
    pub fn was_stopped(&self) -> bool {
        self.0
            .as_ref()
            .is_some_and(|s| s.stopped() == Some(Stop::Stopped))
    }
    pub fn guard(&self) -> CaptureGuard {
        CaptureGuard(self.clone())
    }
    fn enqueue(&self, event: Payload, bytes: usize) {
        let Some(signal) = &self.0 else {
            return;
        };
        let side = match &event {
            Payload::Bytes(side, ..) => Some(*side),
            _ => None,
        };
        let failed = || {
            if let Some(side) = side {
                signal.stop_body(side, Stop::QueueFull);
            } else {
                signal.stop(Stop::QueueFull);
            }
        };
        if signal
            .owner
            .queued
            .fetch_update(Ordering::AcqRel, Ordering::Relaxed, |n| {
                n.checked_add(bytes)
                    .filter(|n| *n <= signal.owner.queue_limit.load(Ordering::Acquire))
            })
            .is_err()
        {
            failed();
            return;
        }
        signal.pending.fetch_add(1, Ordering::Relaxed);
        let envelope = Envelope {
            signal: signal.clone(),
            bytes,
            event,
        };
        if signal
            .owner
            .sender
            .try_send(Event::Capture(envelope))
            .is_err()
        {
            failed();
        }
    }
    pub fn response(&self, status: u16, url: &str, headers: &http::HeaderMap) {
        let Some(signal) = &self.0 else {
            return;
        };
        if !self.is_active() {
            return;
        }
        let body = observed_body(headers);
        signal.binary[1].store(body.binary, Ordering::Relaxed);
        let headers = match masking::headers(headers, signal.hide) {
            Ok(h) => h,
            Err(_) => {
                signal.limit_metadata();
                vec![]
            }
        };
        let response = Message {
            status: Some(status),
            url: url.chars().take(8192).collect(),
            headers,
            body,
            method: None,
        };
        // One bounded response slot per admitted capture survives a full body queue.
        *signal.response.lock().unwrap() = Some(response);
        signal.dirty.store(true, Ordering::Release);
        let _ = signal.owner.sender.try_send(Event::Wake);
    }
    pub fn mark_partial(&self) {
        if let Some(signal) = &self.0 {
            signal.partial.store(true, Ordering::Relaxed);
            signal.dirty.store(true, Ordering::Release);
        }
    }
    pub fn representation(&self, side: Side, value: &'static str) {
        if self.is_active() {
            if value.starts_with("encoded-") {
                if let Some(signal) = &self.0 {
                    signal.binary[side.index()].store(true, Ordering::Relaxed);
                }
            }
            if let Some(signal) = &self.0 {
                signal.representations.lock().unwrap()[side.index()] = Some(value);
                signal.dirty.store(true, Ordering::Release);
            }
        }
    }
    pub fn append(&self, side: Side, bytes: &[u8]) {
        self.append_message(side, bytes, None);
    }
    pub fn append_message(&self, side: Side, bytes: &[u8], kind: Option<&str>) {
        let Some(signal) = &self.0 else {
            return;
        };
        if !signal.owner.enabled.load(Ordering::Relaxed) {
            signal.stop(Stop::Stopped);
            return;
        }
        signal.observed[side.index()].fetch_add(bytes.len() as u64, Ordering::Relaxed);
        signal.dirty.store(true, Ordering::Release);
        if !self.is_active() || signal.body_stopped(side).is_some() {
            return;
        }
        let is_binary = if signal.websocket {
            kind == Some("binary")
                || (matches!(kind, Some("ping" | "pong")) && std::str::from_utf8(bytes).is_err())
        } else {
            signal.binary[side.index()].load(Ordering::Relaxed)
        };
        let eligible = if is_binary && signal.binary_limit == 0 {
            if !bytes.is_empty() {
                signal.skipped[side.index()].store(true, Ordering::Relaxed);
            }
            0
        } else if is_binary {
            let prior = signal.binary_admitted[side.index()].fetch_add(bytes.len() as u64, Ordering::Relaxed);
            bytes.len().min(signal.binary_limit.saturating_sub(prior) as usize)
        } else {
            bytes.len()
        };
        let prior = signal.admitted[side.index()].fetch_add(eligible as u64, Ordering::Relaxed);
        let count = eligible.min(signal.limit.saturating_sub(prior) as usize);
        if is_binary && count > 0 {
            signal.binary[side.index()].store(true, Ordering::Relaxed);
        }
        if count == 0 && kind.is_some() {
            self.enqueue(
                Payload::Bytes(
                    side,
                    vec![],
                    signal.start.elapsed().as_millis() as u64,
                    kind.map(|k| k.chars().take(32).collect()),
                ),
                128,
            );
        }
        for chunk in bytes[..count].chunks(CHUNK_BYTES) {
            if !self.is_active() || signal.body_stopped(side).is_some() {
                break;
            }
            self.enqueue(
                Payload::Bytes(
                    side,
                    chunk.to_vec(),
                    signal.start.elapsed().as_millis() as u64,
                    kind.map(|k| k.chars().take(32).collect()),
                ),
                chunk.len() + 128,
            );
        }
        if count < bytes.len() && (!is_binary || signal.binary_limit != 0) {
            signal.limited[side.index()].store(true, Ordering::Relaxed);
        }
    }
    pub fn finish(&self, state: &str) {
        if let Some(signal) = &self.0 {
            if signal
                .elapsed
                .compare_exchange(
                    0,
                    (signal.start.elapsed().as_millis() as u64).saturating_add(1),
                    Ordering::AcqRel,
                    Ordering::Relaxed,
                )
                .is_ok()
            {
                signal.outcome.store(
                    match state {
                        "complete" => 1,
                        "error" => 2,
                        _ => 3,
                    },
                    Ordering::Release,
                );
                signal.dirty.store(true, Ordering::Release);
                let _ = signal.owner.sender.try_send(Event::Wake);
            }
        }
    }
}
#[cfg(test)]
mod tests;
