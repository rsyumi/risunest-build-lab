use super::*;
use axum::{
    body::Body,
    extract::{Request, State},
    middleware::Next,
    response::Response,
};
use std::{
    collections::BTreeMap,
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicU16, AtomicU64, Ordering},
        mpsc, Mutex,
    },
    task::{Context, Poll},
    time::{Duration, Instant},
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

struct Rate {
    timer: Pin<Box<tokio::time::Sleep>>,
    bytes_per_second: u64,
    next: tokio::time::Instant,
}
impl Rate {
    fn new(bytes_per_second: u64) -> Self {
        Self {
            timer: Box::pin(tokio::time::sleep(Duration::ZERO)),
            bytes_per_second,
            next: tokio::time::Instant::now(),
        }
    }
    fn ready(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        if self.bytes_per_second == 0 {
            Poll::Ready(())
        } else {
            self.timer.as_mut().poll(cx)
        }
    }
    fn used(&mut self, bytes: usize) {
        if self.bytes_per_second > 0 {
            let interval = Duration::from_secs_f64(bytes as f64 / self.bytes_per_second as f64);
            // Keep an absolute byte clock, with at most one packet of credit,
            // so timer quantization does not accumulate on every read.
            self.next = self.next.max(tokio::time::Instant::now() - interval) + interval;
            self.timer.as_mut().reset(self.next);
        }
    }
}
struct Socket {
    socket: tokio::net::TcpStream,
    read: Rate,
    write: Rate,
    bytes: Arc<AtomicU64>,
}
impl AsyncRead for Socket {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        output: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let this = self.get_mut();
        if this.read.ready(cx).is_pending() {
            return Poll::Pending;
        }
        let limit = output.remaining().min(64 * 1024);
        let mut buffer = ReadBuf::new(output.initialize_unfilled_to(limit));
        let result = Pin::new(&mut this.socket).poll_read(cx, &mut buffer);
        let count = buffer.filled().len();
        output.advance(count);
        this.read.used(count);
        this.bytes.fetch_add(count as u64, Ordering::Relaxed);
        result
    }
}
impl AsyncWrite for Socket {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        if this.write.ready(cx).is_pending() {
            return Poll::Pending;
        }
        let result =
            Pin::new(&mut this.socket).poll_write(cx, &bytes[..bytes.len().min(64 * 1024)]);
        if let Poll::Ready(Ok(count)) = result {
            this.write.used(count);
            this.bytes.fetch_add(count as u64, Ordering::Relaxed);
        }
        result
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().socket).poll_flush(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().socket).poll_shutdown(cx)
    }
}
struct Listener {
    listener: tokio::net::TcpListener,
    rate: u64,
    bytes: Arc<AtomicU64>,
}
impl axum::serve::Listener for Listener {
    type Io = Socket;
    type Addr = std::net::SocketAddr;
    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        let (socket, address) = self.listener.accept().await.unwrap();
        socket.set_nodelay(true).unwrap();
        (
            Socket {
                socket,
                read: Rate::new(self.rate),
                write: Rate::new(self.rate),
                bytes: self.bytes.clone(),
            },
            address,
        )
    }
    fn local_addr(&self) -> std::io::Result<Self::Addr> {
        self.listener.local_addr()
    }
}
#[derive(Default)]
struct Faults {
    frame_code: AtomicU16,
    frame_lengths: Mutex<Vec<usize>>,
    code: AtomicU16,
    after_accept: bool,
    latency: Duration,
    chunks: Mutex<BTreeMap<String, usize>>,
    requests: AtomicU64,
    paths: Mutex<BTreeMap<String, usize>>,
    uploads_active: AtomicU64,
    downloads_active: AtomicU64,
    uploads_peak: AtomicU64,
    downloads_peak: AtomicU64,
    range_code: AtomicU16,
    ranges: Mutex<BTreeMap<String, usize>>,
    upload_gate: Option<Rendezvous>,
    download_gate: Option<Rendezvous>,
}
/// Holds the first two requests of one transfer phase until the test
/// releases them, and records any further request admitted meanwhile.
struct Rendezvous {
    admitted: AtomicU64,
    overlapping: AtomicU64,
    entered: mpsc::Sender<u64>,
    release: tokio::sync::Semaphore,
}
impl Rendezvous {
    fn new() -> (Self, mpsc::Receiver<u64>) {
        let (entered, entries) = mpsc::channel();
        let gate = Self {
            admitted: AtomicU64::new(0),
            overlapping: AtomicU64::new(0),
            entered,
            release: tokio::sync::Semaphore::new(0),
        };
        (gate, entries)
    }
    async fn enter(&self) {
        let order = self.admitted.fetch_add(1, Ordering::SeqCst);
        if order < 2 {
            let _ = self.entered.send(order);
            // Closing the semaphore is the release; acquire then fails at once.
            let _ = self.release.acquire().await;
        } else if !self.release.is_closed() {
            self.overlapping.fetch_add(1, Ordering::SeqCst);
            let _ = self.entered.send(order);
        }
    }
    fn release(&self) {
        self.release.close();
    }
}
/// Releases a held phase on every exit path, including a failed assertion.
struct Release<'a>(&'a Rendezvous);
impl Drop for Release<'_> {
    fn drop(&mut self) {
        self.0.release();
    }
}
/// Only detects deadlock. It stays below the client's 30 s request timeout so
/// a stuck phase fails here instead of as a transport error.
const RENDEZVOUS_WATCHDOG: Duration = Duration::from_secs(20);
/// Runs one transfer phase on a worker thread while two of its requests are
/// held at the fixture, then releases them.
fn held_phase(
    phase: &str,
    gate: &Rendezvous,
    entries: &mpsc::Receiver<u64>,
    work: impl FnOnce() + Send,
) {
    std::thread::scope(|scope| {
        let release = Release(gate);
        let worker = scope.spawn(work);
        for held in 0..2 {
            if entries.recv_timeout(RENDEZVOUS_WATCHDOG).is_err() {
                panic!(
                    "{phase}: only {held} of two concurrent requests reached the fixture within {RENDEZVOUS_WATCHDOG:?}"
                );
            }
        }
        // Both requests are now held open. A client that exceeded two in
        // flight would issue its next request without waiting for them; a
        // short window can only miss such a request, never fail a correct one.
        if let Ok(order) = entries.recv_timeout(Duration::from_millis(100)) {
            panic!("{phase}: request {order} was admitted while two were held");
        }
        drop(release);
        worker.join().unwrap();
    });
    assert_eq!(
        gate.overlapping.load(Ordering::SeqCst),
        0,
        "{phase}: a request was admitted while two were held"
    );
}
struct InFlight<'a>(&'a AtomicU64);
impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}
async fn proxy(State(state): State<Arc<Faults>>, request: Request, next: Next) -> Response {
    state.requests.fetch_add(1, Ordering::Relaxed);
    *state.paths.lock().unwrap().entry(request.uri().path().to_owned()).or_default() += 1;
    let counters = if request.uri().path().contains("/chunks/") || request.uri().path() == "/uploads/frames" {
        Some((&state.uploads_active, &state.uploads_peak))
    } else if request.method().as_str() == "GET" && request.uri().path().starts_with("/objects/") {
        Some((&state.downloads_active, &state.downloads_peak))
    } else {
        None
    };
    let _inflight = counters.map(|(active, peak)| {
        let count = active.fetch_add(1, Ordering::Relaxed) + 1;
        peak.fetch_max(count, Ordering::Relaxed);
        InFlight(active)
    });
    let gate = if request.method().as_str() == "PUT" && request.uri().path().contains("/chunks/") {
        state.upload_gate.as_ref()
    } else if request.method().as_str() == "GET"
        && request.uri().path().starts_with("/objects/")
        && request.headers().contains_key("range")
    {
        state.download_gate.as_ref()
    } else {
        None
    };
    if let Some(gate) = gate {
        gate.enter().await;
    }
    tokio::time::sleep(state.latency).await;
    let path = request.uri().path();
    if path == "/uploads/frames" {
        state.frame_lengths.lock().unwrap().push(request.headers().get("content-length").unwrap().to_str().unwrap().parse().unwrap());
        let code = state.frame_code.swap(0, Ordering::Relaxed);
        if code > 0 {
            axum::body::to_bytes(request.into_body(), risunest_sync_wire::transfer::MAX_BATCH_BYTES).await.unwrap();
            return Response::builder().status(code).header("retry-after", "0").body(Body::empty()).unwrap();
        }
    }
    if path.starts_with("/objects/") && request.method().as_str() == "GET" {
        if let Some(range) = request.headers().get("range") {
            let range = range.to_str().unwrap().to_owned();
            *state
                .ranges
                .lock()
                .unwrap()
                .entry(range.clone())
                .or_default() += 1;
            if range.starts_with("bytes=0-") {
                let code = state.range_code.swap(0, Ordering::Relaxed);
                if code > 0 {
                    return Response::builder()
                        .status(code)
                        .header("retry-after", "1")
                        .body(Body::empty())
                        .unwrap();
                }
            }
        }
    }
    if path.contains("/chunks/") {
        let index = path.rsplit('/').next().unwrap().to_owned();
        *state
            .chunks
            .lock()
            .unwrap()
            .entry(index.clone())
            .or_default() += 1;
        if index == "1" {
            let code = state.code.swap(0, Ordering::Relaxed);
            if code > 0 {
                if state.after_accept {
                    let _ = next.run(request).await;
                } else {
                    // Model a received HTTP error. Dropping the request while
                    // reqwest is writing produces a TCP reset instead (503),
                    // which is a separate transport failure.
                    axum::body::to_bytes(
                        request.into_body(),
                        risunest_sync_wire::transfer::MAX_BATCH_BYTES,
                    )
                    .await
                    .unwrap();
                }
                return Response::builder()
                    .status(code)
                    .header("retry-after", "1")
                    .body(Body::empty())
                    .unwrap();
            }
        }
    }
    next.run(request).await
}
fn run_case(
    rate: u64,
    latency: Duration,
    failure: u16,
    after_accept: bool,
    download_failure: u16,
    hold_two: bool,
) -> (u64, u64, u128, u128, u64, u64) {
    let root = tempfile::tempdir().unwrap();
    let server = Arc::new(Store::init(root.path()).unwrap());
    let device = server.add_device().unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let listener = runtime
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let bytes = Arc::new(AtomicU64::new(0));
    let (upload_gate, download_gate, entries) = if hold_two {
        let (upload, upload_entries) = Rendezvous::new();
        let (download, download_entries) = Rendezvous::new();
        (Some(upload), Some(download), Some((upload_entries, download_entries)))
    } else {
        (None, None, None)
    };
    let faults = Arc::new(Faults {
        code: AtomicU16::new(failure),
        after_accept,
        latency,
        range_code: AtomicU16::new(download_failure),
        upload_gate,
        download_gate,
        ..Default::default()
    });
    let faults_for_router = faults.clone();
    let counted = bytes.clone();
    let task = runtime.spawn(async move {
        let router = http::router(server).layer(axum::middleware::from_fn_with_state(
            faults_for_router,
            proxy,
        ));
        axum::serve(
            Listener {
                listener,
                rate,
                bytes: counted,
            },
            router,
        )
        .await
        .unwrap();
    });
    let client = ServerClient::new(ServerConfig {
        directory: None,
        endpoint,
        library_id: device.library_id,
        device_id: device.device_id,
        token: device.token,
    })
    .unwrap();
    let source_dir = tempfile::tempdir().unwrap();
    let target_dir = tempfile::tempdir().unwrap();
    let source = Cache::open(source_dir.path()).unwrap();
    let target = Cache::open(target_dir.path()).unwrap();
    let payload: Vec<_> = (0..4 * risunest_sync_wire::transfer::UPLOAD_CHUNK_BYTES + 17)
        .map(|i| (i.wrapping_mul(137) % 251) as u8)
        .collect();
    let hash = source.put(&payload).unwrap();
    let started = Instant::now();
    if let Some((upload_entries, _)) = &entries {
        held_phase(
            "upload",
            faults.upload_gate.as_ref().unwrap(),
            upload_entries,
            || {
                Transfer::new(&client, &source)
                    .unwrap()
                    .upload(std::slice::from_ref(&hash), &[])
                    .unwrap()
            },
        );
    } else {
        let transfer = Transfer::new(&client, &source).unwrap();
        if failure > 0 {
            let error = transfer
                .upload(std::slice::from_ref(&hash), &[])
                .unwrap_err();
            assert_eq!(
                error.status,
                failure,
                "{error:?}, attempts={:?}",
                faults.chunks.lock().unwrap()
            );
        } else {
            transfer.upload(std::slice::from_ref(&hash), &[]).unwrap();
        }
    }
    if failure > 0 {
        // Reopen the durable native journal, exactly as after process restart.
        Transfer::new(&client, &source)
            .unwrap()
            .upload(std::slice::from_ref(&hash), &[])
            .unwrap();
        let attempts = faults.chunks.lock().unwrap();
        assert_eq!(attempts.get("0"), Some(&1));
        assert_eq!(attempts.get("1"), Some(&if after_accept { 1 } else { 2 }));
    }
    let upload_ms = started.elapsed().as_millis();
    let started = Instant::now();
    if download_failure > 0 {
        let error = Transfer::new(&client, &target)
            .unwrap()
            .download(std::slice::from_ref(&hash), &[])
            .unwrap_err();
        assert_eq!(error.status, download_failure);
    }
    let download = || {
        Transfer::new(&client, &target)
            .unwrap()
            .download(std::slice::from_ref(&hash), &[])
            .unwrap()
    };
    if let Some((_, download_entries)) = &entries {
        held_phase(
            "download",
            faults.download_gate.as_ref().unwrap(),
            download_entries,
            download,
        );
    } else {
        download();
    }
    let download_ms = started.elapsed().as_millis();
    assert_eq!(target.read(&hash, payload.len()).unwrap(), payload);
    if hold_two {
        // Every full chunk and the tail passed through each phase's gate.
        let chunks = payload
            .len()
            .div_ceil(risunest_sync_wire::transfer::UPLOAD_CHUNK_BYTES) as u64;
        for (phase, gate) in [("upload", &faults.upload_gate), ("download", &faults.download_gate)] {
            let admitted = gate.as_ref().unwrap().admitted.load(Ordering::SeqCst);
            assert_eq!(admitted, chunks, "{phase} chunk requests");
        }
    }
    if download_failure > 0 {
        let ranges = faults.ranges.lock().unwrap();
        assert_eq!(ranges.get("bytes=0-1048575"), Some(&2));
        // The second parallel response was persisted even though the first
        // request failed, and is reused after reopening the native journal.
        assert_eq!(ranges.get("bytes=1048576-2097151"), Some(&1));
        assert_eq!(ranges.get("bytes=4194304-4194320"), Some(&1));
    }
    let result = (
        bytes.load(Ordering::Relaxed),
        faults.requests.load(Ordering::Relaxed),
        upload_ms,
        download_ms,
        faults.uploads_peak.load(Ordering::Relaxed),
        faults.downloads_peak.load(Ordering::Relaxed),
    );
    task.abort();
    runtime.shutdown_timeout(Duration::from_secs(2));
    result
}
#[test]
fn full_chunk_upload_and_download_overlap_exactly_two_requests() {
    let result = run_case(0, Duration::ZERO, 0, false, 0, true);
    assert_eq!(result.4, 2, "upload concurrency");
    assert_eq!(result.5, 2, "download concurrency");
}
#[test]
fn proxy_413_429_524_resume_only_unverified_chunks_and_lost_accept_is_not_resent() {
    for code in [413, 429, 524] {
        run_case(0, Duration::ZERO, code, false, 0, false);
    }
    run_case(0, Duration::ZERO, 524, true, 0, false);
}
#[test]
fn failed_range_keeps_the_verified_parallel_response_across_reopen() {
    for code in [413, 429, 524] {
        run_case(0, Duration::from_millis(20), 0, false, code, false);
    }
}
#[test]
#[ignore = "Explicit synthetic bandwidth and request latency measurement"]
fn slow_network_full_transfer_gate() {
    for (bits, millis) in [(10_000_000, 80), (1_000_000, 200)] {
        let result = run_case(bits / 8, Duration::from_millis(millis), 0, false, 0, false);
        eprintln!("synthetic bits/sec={bits}, request delay={millis}ms: (HTTP bytes, requests, upload ms, download ms, upload concurrency, download concurrency)={result:?}");
        assert!(result.0 < 8 * 1024 * 1024 + 64 * 1024);
    }
}

#[test]
#[ignore = "Explicit synthetic cross-record transfer measurement"]
fn bounded_record_upload_measurement() {
    for latency in [0, 50] {
        for batched in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let server = Arc::new(Store::init(root.path()).unwrap());
            let device = server.add_device().unwrap();
            let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
            let listener = runtime.block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).unwrap();
            let endpoint = format!("http://{}", listener.local_addr().unwrap());
            let faults = Arc::new(Faults { latency: Duration::from_millis(latency), ..Default::default() });
            let served = server.clone();
            let observed = faults.clone();
            let task = runtime.spawn(async move {
                let router = http::router(served).layer(axum::middleware::from_fn_with_state(observed, proxy));
                axum::serve(listener, router).await.unwrap();
            });
            let client = ServerClient::new(ServerConfig { directory: None, endpoint, library_id: device.library_id, device_id: device.device_id, token: device.token }).unwrap();
            let local = tempfile::tempdir().unwrap();
            let cache = Cache::open(local.path()).unwrap();
            let records = (0..200).map(|n| cache.project_bytes(format!("synthetic record {n:04}").as_bytes(), &[], &[], vec![]).unwrap()).collect::<Vec<_>>();
            let transfer = Transfer::new(&client, &cache).unwrap();
            let started = Instant::now();
            if batched {
                let objects = records.iter().flat_map(|r| r.objects.clone()).collect::<std::collections::BTreeSet<_>>().into_iter().collect::<Vec<_>>();
                transfer.upload(&objects, &[]).unwrap();
            } else {
                for record in &records { transfer.upload(&record.objects, &[]).unwrap(); }
            }
            let elapsed = started.elapsed().as_millis();
            for record in records { for hash in record.objects { assert_eq!(server.get_object(&hash).unwrap(), cache.read(&hash, 1024 * 1024).unwrap()); } }
            let paths = faults.paths.lock().unwrap();
            eprintln!("records=200 delay_ms={latency} batched={batched} elapsed_ms={elapsed} missing={} frames={} requests={}", paths.get("/objects/missing").unwrap_or(&0), paths.get("/uploads/frames").unwrap_or(&0), faults.requests.load(Ordering::Relaxed));
            if batched { assert_eq!(paths.get("/objects/missing"), Some(&1)); assert_eq!(paths.get("/uploads/frames"), Some(&1)); }
            task.abort();
            runtime.shutdown_timeout(Duration::from_secs(2));
        }
    }
}

fn frame_case(sizes: &[usize], depth: usize, rate: u64, failure: u16, mixed_bases: bool) {
    let root = tempfile::tempdir().unwrap();
    let server = Arc::new(Store::init(root.path()).unwrap());
    let device = server.add_device().unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let listener = runtime.block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let faults = Arc::new(Faults { frame_code: AtomicU16::new(failure), latency: Duration::from_millis(50), ..Default::default() });
    let served = server.clone();
    let observed = faults.clone();
    let wire = Arc::new(AtomicU64::new(0));
    let counted = wire.clone();
    let task = runtime.spawn(async move {
        let router = http::router(served).layer(axum::middleware::from_fn_with_state(observed, proxy));
        axum::serve(Listener { listener, rate, bytes: counted }, router).await.unwrap();
    });
    let client = ServerClient::new(ServerConfig { directory: None, endpoint, library_id: device.library_id, device_id: device.device_id, token: device.token }).unwrap();
    let local = tempfile::tempdir().unwrap();
    let cache = Cache::open(local.path()).unwrap();
    let hashes = sizes.iter().enumerate().map(|(i, size)| cache.put(&vec![(i + 1) as u8; *size]).unwrap()).collect::<Vec<_>>();
    let transfer = Transfer::new(&client, &cache).unwrap().with_frame_depth(depth);
    let mut targets = Vec::new();
    if mixed_bases {
        for (i, hash) in hashes.iter().enumerate() {
            let mut base = cache.read(hash, sizes[i]).unwrap();
            base[0] = 42;
            let base = cache.put(&base).unwrap();
            if i < 2 { transfer.upload(std::slice::from_ref(&base), &[]).unwrap(); }
            targets.push(crate::server_sync::transfer::UploadTarget { hash: hash.clone(), bases: vec![base].into(), base_lease: i == 0 });
        }
    }
    let started = Instant::now();
    if mixed_bases { transfer.upload_targets(&targets).unwrap(); }
    else { transfer.upload(&hashes, &[]).unwrap(); }
    let elapsed = started.elapsed().as_millis();
    for (hash, size) in hashes.iter().zip(sizes) { assert_eq!(server.get_object(hash).unwrap(), cache.read(hash, *size).unwrap()); }
    let peak = faults.uploads_peak.load(Ordering::Relaxed);
    if rate > 0 { assert!(transfer.frame_byte_limit() < 2 * 1024 * 1024); }
    eprintln!("adapted_frame_limit={}", transfer.frame_byte_limit());
    assert!(peak <= 2);
    let lengths = faults.frame_lengths.lock().unwrap();
    assert!(lengths.iter().all(|size| *size <= 4 * 1024 * 1024 + 53));
    if failure > 0 { assert!(lengths.iter().skip(2).any(|size| *size < lengths[0])); }
    if sizes.iter().all(|size| *size < 4 * 1024 * 1024) && failure == 0 && rate == 0 && !mixed_bases { assert_eq!(peak, depth as u64); }
    eprintln!("sizes={sizes:?} depth={depth} rate={rate} failure={failure} elapsed_ms={elapsed} wire_bytes={} requests={} peak={peak} frames={lengths:?} paths={:?}", wire.load(Ordering::Relaxed), faults.requests.load(Ordering::Relaxed), faults.paths.lock().unwrap());
    task.abort();
    runtime.shutdown_timeout(Duration::from_secs(2));
}

#[test]
fn frame_batches_join_two_requests_and_shrink_after_transient_failure() {
    frame_case(&[1024 * 1024; 8], 2, 0, 503, false);
}

#[test]
fn one_batch_keeps_leased_unleased_and_missing_base_contexts() {
    frame_case(&[64 * 1024; 3], 2, 0, 0, true);
}

#[test]
#[ignore = "Explicit synthetic frame sizing and concurrency measurement"]
fn frame_sizing_measurement() {
    for depth in [1, 2] { frame_case(&[1024 * 1024; 8], depth, 0, 0, false); }
    frame_case(&[256 * 1024, 1024 * 1024, 2 * 1024 * 1024, 3 * 1024 * 1024, 4 * 1024 * 1024, 8 * 1024 * 1024], 2, 0, 0, false);
    frame_case(&[1024 * 1024; 8], 2, 32 * 1024, 0, false);
}

/// §6.3 and U3. Two records that name the same references share the pages that
/// describe them, so a receive that follows both roots in one walk downloads a
/// shared subtree once instead of rejecting the second sight of it. The upload
/// side's hint walk already skipped a page it had seen.
#[test]
fn one_walk_follows_two_roots_and_downloads_their_shared_pages_once() {
    use crate::server_sync::{cache::Cache, transfer::Transfer};
    use risunest_sync_wire::{descriptor::build_reference_tree, hash};
    let root = tempfile::tempdir().unwrap();
    let server = Arc::new(Store::init(root.path()).unwrap());
    let credential = server.add_device().unwrap();
    let device = server.authenticate(&credential.library_id, &credential.token).unwrap();
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let listener = runtime.block_on(tokio::net::TcpListener::bind("127.0.0.1:0")).unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let faults = Arc::new(Faults::default());
    let served = server.clone();
    let observed = faults.clone();
    let task = runtime.spawn(async move {
        let router = http::router(served).layer(axum::middleware::from_fn_with_state(observed, proxy));
        axum::serve(Listener { listener, rate: 0, bytes: Arc::new(AtomicU64::new(0)) }, router).await.unwrap();
    });
    let client = ServerClient::new(ServerConfig {
        directory: None, endpoint, library_id: credential.library_id,
        device_id: credential.device_id, token: credential.token,
    }).unwrap();

    // Two dependency sets whose leading pages are identical, so their trees
    // share every page below the one the differing tail lands in.
    let mut shared: Vec<_> = (0..4_000u64)
        .map(|index| hash(format!("shared dependency {index:016}").as_bytes()))
        .collect();
    shared.sort();
    let mut tail = shared.clone();
    tail.pop();
    tail.push(hash(b"one dependency only the second record has"));
    tail.sort();
    let (first_root, first_pages) = build_reference_tree(&shared, false).unwrap();
    let (second_root, second_pages) = build_reference_tree(&tail, false).unwrap();
    let first_root = first_root.unwrap();
    let second_root = second_root.unwrap();
    assert_ne!(first_root, second_root);
    for (digest, bytes) in first_pages.iter().chain(second_pages.iter()) {
        server.put_object(&device, digest, bytes).unwrap();
    }

    let separate = tempfile::tempdir().unwrap();
    let separate_cache = Cache::open(separate.path()).unwrap();
    faults.requests.store(0, Ordering::Relaxed);
    let transfer = Transfer::new(&client, &separate_cache).unwrap();
    transfer.download_reference_tree(vec![(first_root.clone(), vec![])]).unwrap();
    transfer.download_reference_tree(vec![(second_root.clone(), vec![])]).unwrap();
    let apart = faults.requests.swap(0, Ordering::Relaxed);

    let together = tempfile::tempdir().unwrap();
    let together_cache = Cache::open(together.path()).unwrap();
    let dependencies = Transfer::new(&client, &together_cache).unwrap()
        .download_reference_tree(vec![
            (first_root.clone(), vec![]),
            (second_root.clone(), vec![]),
            // A record naming a root another record already named is the same
            // page twice, which is no longer a reason to refuse the walk.
            (first_root, vec![]),
        ])
        .unwrap();
    let joined = faults.requests.load(Ordering::Relaxed);
    for (digest, bytes) in first_pages.iter().chain(second_pages.iter()) {
        assert_eq!(together_cache.read(digest, risunest_sync_wire::MAX_METADATA_BYTES).unwrap(), *bytes);
    }
    // Both tails are there and the shared body is named once.
    assert_eq!(dependencies.len(), shared.len() + 1);
    eprintln!("reference_roots=3 separate_requests={apart} joined_requests={joined}");
    assert!(joined < apart, "joined {joined} separate {apart}");
    task.abort();
    runtime.shutdown_timeout(Duration::from_secs(2));
}
