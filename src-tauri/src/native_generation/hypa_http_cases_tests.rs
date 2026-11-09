use super::*;
use risunest_memory::hypa_v1::{CacheError, CacheKey, CacheRow, CacheWrite, EmbeddingTarget, HypaV1Retrieval, ImmediateCache};
use serde_json::{json, Value};
use std::{collections::BTreeMap, time::Duration};
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::TcpListener, sync::mpsc};

#[path = "hypa_http_additional_cases_tests.rs"]
mod additional;
#[path = "hypa_http_tail_cases_tests.rs"]
mod tail;
#[path = "hypa_http_native_cache_tests.rs"]
mod native_cache;

const ORIGIN: &str = "https://synthetic.example.invalid";
const JSON: &[u8] = br#"{"data":[{"embedding":[1.00000001,2]}]}"#;

fn limits() -> Limits {
    Limits { url_bytes: 16 * 1024, header_bytes: 8192, response_bytes: 256 * 1024,
        decode_owned: 256 * 1024, decode_work: 8 * 1024 * 1024, copy_quantum: 127,
        decode: DecodeLimits { input_quantum: 1024, output_quantum: 257, max_encoded_bytes: 256 * 1024,
            max_layer_decoded_bytes: 256 * 1024, max_total_decoded_bytes: 256 * 1024, max_layers: 1,
            max_header_bytes: 8192, max_window_sum: 32768, zstd_window_log_max: 10 } }
}

pub(super) fn operation() -> (Operation, Budget, Arc<Semaphore>) {
    let cancel = Cancellation::new();
    let budget = Budget::new(risunest_ir::Limits { max_owned_bytes: 4 * 1024 * 1024, max_json_bytes: 1024 * 1024,
        max_text_units: 1024 * 1024, max_values: 100_000, max_depth: 32, max_work: 64 * 1024 * 1024 }, cancel.flag()).unwrap();
    let slots = Arc::new(Semaphore::new(1));
    (Operation::admit(slots.clone(), budget.clone(), cancel).unwrap(), budget, slots)
}

fn request(url: String) -> HttpRequestSpec {
    HttpRequestSpec { url, method: HttpMethod::Post, headers: vec![("Content-Type".into(), "application/json".into())],
        body: br#"{"input":"synthetic"}"#.to_vec(), response: ResponseKind::Buffered }
}

#[derive(Clone)]
struct Reply {
    status: u16,
    headers: Vec<(String, Vec<u8>)>,
    chunks: Vec<Vec<u8>>,
    held: bool,
    held_headers: bool,
    pause_after_first: Option<Arc<Notify>>,
    tail_after_release: Vec<u8>,
}
impl Reply {
    fn plain(status: u16, body: &[u8]) -> Self {
        Self { status, headers: vec![("Content-Type".into(), b"application/json".to_vec())], chunks: vec![body.to_vec()], held: false, held_headers: false, pause_after_first: None, tail_after_release: Vec::new() }
    }
    fn encoded(body: Vec<u8>, coding: &str) -> Self {
        let mut reply = Self::plain(200, &body);
        reply.headers.push(("Content-Encoding".into(), coding.as_bytes().to_vec()));
        reply
    }
}

#[derive(Debug, PartialEq, Eq)]
struct WireRequest { method: String, target: String, headers: BTreeMap<String, Vec<String>>, body: Vec<u8> }

struct Loopback {
    url: String,
    requests: mpsc::Receiver<WireRequest>,
    raw_header_lines: mpsc::Receiver<Vec<String>>,
    release: Arc<Notify>,
    worker: tokio::task::JoinHandle<()>,
}
impl Loopback {
    async fn new(replies: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (send, requests) = mpsc::channel(replies.len().max(1));
        let (send_raw_headers, raw_header_lines) = mpsc::channel(replies.len().max(1));
        let release = Arc::new(Notify::new());
        let released = release.clone();
        let worker = tokio::spawn(async move {
            for reply in replies {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut wire = Vec::new();
                let end = loop {
                    if let Some(at) = wire.windows(4).position(|part| part == b"\r\n\r\n") { break at + 4; }
                    assert!(wire.len() < 64 * 1024, "synthetic request header bound");
                    assert_ne!(stream.read_buf(&mut wire).await.unwrap(), 0);
                };
                let header = std::str::from_utf8(&wire[..end]).unwrap();
                let mut lines = header.split("\r\n");
                let mut start = lines.next().unwrap().split(' ');
                let method = start.next().unwrap().to_owned();
                let target = start.next().unwrap().to_owned();
                let mut headers = BTreeMap::<String, Vec<String>>::new();
                let mut raw_headers = Vec::new();
                for line in lines.filter(|line| !line.is_empty()) {
                    raw_headers.push(line.to_owned());
                    let (name, value) = line.split_once(':').unwrap();
                    headers.entry(name.to_ascii_lowercase()).or_default().push(value.trim().to_owned());
                }
                let length = headers.get("content-length").map_or(0, |values| values[0].parse::<usize>().unwrap());
                assert!(length <= 1024 * 1024, "synthetic request body bound");
                while wire.len() - end < length { assert_ne!(stream.read_buf(&mut wire).await.unwrap(), 0); }
                send_raw_headers.send(raw_headers).await.unwrap();
                send.send(WireRequest { method, target, headers, body: wire[end..end + length].to_vec() }).await.unwrap();
                if reply.held_headers { released.notified().await; }
                let mut response = format!("HTTP/1.1 {} Synthetic\r\nConnection: close\r\n", reply.status).into_bytes();
                for (name, value) in reply.headers { response.extend_from_slice(name.as_bytes()); response.extend_from_slice(b": "); response.extend(value); response.extend_from_slice(b"\r\n"); }
                if reply.held { response.extend_from_slice(b"Transfer-Encoding: chunked\r\n\r\n"); }
                else { response.extend_from_slice(format!("Content-Length: {}\r\n\r\n", reply.chunks.iter().map(Vec::len).sum::<usize>()).as_bytes()); }
                if stream.write_all(&response).await.is_err() { continue; }
                for (index, chunk) in reply.chunks.into_iter().enumerate() {
                    if reply.held && stream.write_all(format!("{:x}\r\n", chunk.len()).as_bytes()).await.is_err() { break; }
                    if stream.write_all(&chunk).await.is_err() { break; }
                    if reply.held && stream.write_all(b"\r\n").await.is_err() { break; }
                    if index == 0 { if let Some(release) = &reply.pause_after_first { release.notified().await; } }
                }
                if reply.held {
                    released.notified().await;
                    let _ = stream.write_all(&reply.tail_after_release).await;
                }
            }
        });
        Self { url, requests, raw_header_lines, release, worker }
    }
    async fn captured(&mut self) -> WireRequest {
        tokio::time::timeout(Duration::from_secs(10), self.requests.recv()).await.unwrap().unwrap()
    }
    async fn captured_raw_headers(&mut self) -> Vec<String> {
        tokio::time::timeout(Duration::from_secs(10), self.raw_header_lines.recv()).await.unwrap().unwrap()
    }
}
impl Drop for Loopback {
    fn drop(&mut self) { self.release.notify_one(); self.worker.abort(); }
}

#[derive(Debug, PartialEq, Eq)]
struct WireReply { status: u16, body: Vec<u8> }

async fn native_plugin(url: String, body: Vec<u8>, headers: Vec<(String, String)>, operation: Operation) -> Result<WireReply, &'static str> {
    // The whole blocking IPC sequence owns admission until its final body/cancel call settles.
    let job = tokio::task::spawn_blocking(move || {
        let _operation = operation;
        let window = plugin::http_window_with_client_builder(tauri::utils::platform::Target::Windows,
            |builder| builder.no_proxy().timeout(Duration::from_secs(5)));
        let rid = plugin::invoke(&window, "fetch", json!({"clientConfig": {
            "url": url, "method": "POST", "headers": headers, "data": null, "dataText": String::from_utf8(body).unwrap(),
            "maxRedirections": 10
        }})).map_err(|_| "fetch")?.deserialize::<u32>().map_err(|_| "fetch-ipc")?;
        let response = match plugin::invoke(&window, "fetch_send", json!({"rid": rid})) {
            Ok(value) => value.deserialize::<Value>().map_err(|_| "send-ipc")?,
            Err(_) => { let _ = plugin::invoke(&window, "fetch_cancel", json!({"rid": rid})); return Err("send"); }
        };
        let status = response["status"].as_u64().ok_or("status")? as u16;
        // These two response-constructor boundaries are independently observed in actual plugin JS.
        let invalid_header = response["headers"].as_array().ok_or("headers")?.iter()
            .any(|pair| pair[1].as_str().is_none_or(|value| value.chars().any(|scalar| scalar as u32 > 255)));
        if invalid_header || !(200..=599).contains(&status) || matches!(status, 204 | 205 | 304) {
            let _ = plugin::invoke(&window, "fetch_cancel_body", json!({"rid": response["rid"]}));
            if invalid_header || !(200..=599).contains(&status) { return Err("response-constructor"); }
            return Ok(WireReply { status, body: Vec::new() });
        }
        plugin::read_body_result(&window, &response).map(|body| WireReply { status, body }).map_err(|_| "read")
    });
    tokio::time::timeout(Duration::from_secs(10), job).await.expect("native plugin fixture timed out").unwrap()
}

async fn native_owner(url: String, limits: Limits) -> Result<WireReply, Failure> {
    let (operation, _, _) = operation();
    let mut executor = Executor::new(operation, limits, ORIGIN.into())?;
    let reply = executor.execute(request(url)).await?;
    executor.resume(reply, |reply| match reply {
        HostReply::Http { status, body } => WireReply { status, body },
        _ => unreachable!(),
    })
}

#[tokio::test]
async fn actual_plugin_and_owner_preserve_wire_request_and_buffered_status_bytes() {
    for status in [200, 201, 400, 429, 500] {
        let mut server = Loopback::new(vec![Reply::plain(status, JSON), Reply::plain(status, JSON)]).await;
        let url = format!("{}/v1/embeddings?q=synthetic#ignored", server.url);
        let input = request(url.clone());
        let mut headers = input.headers.clone();
        headers.push(("Origin".into(), ORIGIN.into()));
        let (operation, _, _) = operation();
        let baseline = native_plugin(url.clone(), input.body, headers, operation).await.unwrap();
        let before = server.captured().await;
        let plugin_headers = server.captured_raw_headers().await;
        let actual = native_owner(url, limits()).await.unwrap();
        let after = server.captured().await;
        let executor_headers = server.captured_raw_headers().await;
        assert!(!plugin_headers.is_empty() && !executor_headers.is_empty());
        println!("RISUNEST_HTTP_WIRE_HEADERS {}", json!({"schema": 1, "status": status,
            "plugin": plugin_headers, "executor": executor_headers}));
        assert_eq!(actual, baseline);
        assert_eq!(after, before);
        assert_eq!(after.target, "/v1/embeddings?q=synthetic");
        assert_eq!(after.headers["accept-encoding"], ["gzip"]);
        assert_eq!(after.headers["user-agent"], ["tauri-plugin-http/2.8.0"]);
    }
}

#[tokio::test]
async fn actual_plugin_and_owner_use_first_coding_field_and_strict_gzip() {
    let gzip = decode::tests::encode(decode::Coding::Gzip, JSON);
    let mut crc = gzip.clone(); let crc_at = crc.len() - 8; crc[crc_at] ^= 1;
    let mut size = gzip.clone(); let size_at = size.len() - 4; size[size_at] ^= 1;
    let mut joined = gzip.clone(); joined.extend_from_slice(&gzip);
    let mut trailing = gzip.clone(); trailing.extend_from_slice(b"trailing");
    let mut both = Reply::encoded(gzip.clone(), "gzip"); both.headers.push(("Content-Encoding".into(), b"br".to_vec()));
    let mut reversed = Reply::encoded(gzip.clone(), "br"); reversed.headers.push(("Content-Encoding".into(), b"gzip".to_vec()));
    let mut removed_header = Reply::encoded(gzip.clone(), "gzip"); removed_header.headers.push(("Content-Encoding".into(), vec![255]));
    let plans = vec![
        ("gzip", Reply::encoded(gzip.clone(), "gzip"), Some(JSON.to_vec())),
        ("crc", Reply::encoded(crc, "gzip"), None),
        ("size", Reply::encoded(size, "gzip"), None),
        ("footer", Reply::encoded(gzip[..gzip.len() - 8].to_vec(), "gzip"), None),
        ("header", Reply::encoded(gzip[..5].to_vec(), "gzip"), None),
        ("members", Reply::encoded(joined, "gzip"), Some(JSON.to_vec())),
        ("trailing", Reply::encoded(trailing, "gzip"), Some(JSON.to_vec())),
        ("first-gzip", both, Some(JSON.to_vec())),
        ("first-br", reversed, Some(gzip.clone())),
        ("removed-invalid-header", removed_header, Some(JSON.to_vec())),
        ("case-sensitive", Reply::encoded(gzip.clone(), "GZIP"), Some(gzip.clone())),
        ("x-gzip", Reply::encoded(gzip.clone(), "x-gzip"), Some(gzip.clone())),
        ("combined", Reply::encoded(gzip.clone(), "gzip, br"), Some(gzip)),
    ];
    for (name, reply, expected) in plans {
        let mut server = Loopback::new(vec![reply.clone(), reply]).await;
        let url = format!("{}/v1/embeddings", server.url);
        let input = request(url.clone());
        let (operation, _, _) = operation();
        let baseline = native_plugin(url.clone(), input.body, input.headers, operation).await;
        server.captured().await;
        let actual = native_owner(url, limits()).await;
        server.captured().await;
        assert_eq!(baseline.as_ref().ok().map(|reply| reply.body.as_slice()), expected.as_deref(), "{name} native baseline");
        assert_eq!(actual.as_ref().ok().map(|reply| reply.body.as_slice()), expected.as_deref(), "{name} private owner");
        assert_eq!(actual.is_err(), baseline.is_err(), "{name}");
    }
}

#[tokio::test]
async fn null_bodies_do_not_wait_for_upstream_eof() {
    let plans = [204, 205, 304].map(|status| Reply::plain(status, JSON));
    for mut reply in plans {
        reply.held = true;
        for baseline in [true, false] {
            let mut server = Loopback::new(vec![reply.clone()]).await;
            let url = format!("{}/v1/embeddings", server.url);
            let result = tokio::time::timeout(Duration::from_secs(3), async {
                if baseline {
                    let input = request(url.clone()); let (operation, _, _) = operation();
                    native_plugin(url, input.body, input.headers, operation).await.unwrap()
                } else { native_owner(url, limits()).await.unwrap() }
            }).await.expect("body completion waited for held-open upstream");
            assert_eq!(result.body, b"");
            server.captured().await;
        }
    }
}

#[tokio::test]
async fn exposed_response_headers_fail_before_json_parsing() {
    for raw in [vec![255], "\u{100}".as_bytes().to_vec()] {
        let mut reply = Reply::plain(200, JSON); reply.headers.push(("X-Synthetic".into(), raw));
        let mut server = Loopback::new(vec![reply.clone(), reply]).await;
        let url = format!("{}/v1/embeddings", server.url);
        let input = request(url.clone()); let (operation, _, _) = operation();
        assert!(native_plugin(url.clone(), input.body, input.headers, operation).await.is_err());
        server.captured().await;
        assert!(native_owner(url, limits()).await.is_err());
        server.captured().await;
    }
}

#[test]
fn bytes_engine_clones_and_header_clones_hold_storage_and_admission() {
    let (operation, budget, slots) = operation();
    let mut data = Vec::with_capacity(4096); data.extend_from_slice(b"synthetic");
    let bytes = ownership::payload(data, 0, 9, &operation).unwrap();
    let header = HeaderValue::from_maybe_shared(ownership::header_payload("Bearer synthetic".into(), &operation).unwrap()).unwrap();
    let mut request = reqwest::Request::new(Method::POST, url::Url::parse("http://127.0.0.1/").unwrap());
    *request.body_mut() = Some(reqwest::Body::from(bytes));
    request.headers_mut().insert(header::AUTHORIZATION, header.clone());
    let replay = request.try_clone().unwrap();
    drop(request); drop(operation);
    assert_eq!(slots.available_permits(), 0);
    assert!(budget.owned_bytes() >= Budget::context_bytes() + 4096);
    drop(replay);
    assert_eq!(slots.available_permits(), 0);
    drop(header);
    assert_eq!(slots.available_permits(), 1);
    assert_eq!(budget.owned_bytes(), Budget::context_bytes());
}

#[test]
fn buffer_replacement_requires_old_and_new_storage_and_releases_on_failure() {
    let (operation, budget, _) = operation();
    let mut buffer = Buffer::new(&operation, 128, 3).unwrap();
    buffer.append(b"12345678").unwrap();
    let before = budget.owned_bytes();
    let only_final_size = budget.reserve_owned(budget.limits().max_owned_bytes - before - 8).unwrap();
    assert!(matches!(buffer.append(b"9"), Err(Failure::Limit(_))));
    assert_eq!(buffer.bytes(), b"12345678");
    assert_eq!(budget.owned_bytes(), budget.limits().max_owned_bytes - 8);
    drop(only_final_size);
    buffer.append(b"9").unwrap();
    assert!(budget.peak_bytes() >= before + 16);
    assert_eq!(buffer.bytes(), b"123456789");
    assert!(buffer.capacity() >= 9);
    drop(buffer); drop(operation);
    assert_eq!(budget.owned_bytes(), Budget::context_bytes());
}

struct Unpause(Arc<ParseBarrier>);
impl Drop for Unpause { fn drop(&mut self) { self.0.release(); } }

#[tokio::test]
async fn explicit_cancel_waits_for_initial_parser_worker_settlement() {
    for at in [PauseAt::BeforeParse, PauseAt::AfterParse] {
        let (operation, budget, slots) = operation();
        let cancel = operation.cancel().clone();
        let barrier = ParseBarrier::new(at);
        let release = Unpause(barrier.clone());
        let worker_barrier = barrier.clone();
        let job = tokio::spawn(async move {
            let mut executor = Executor::new(operation, limits(), ORIGIN.into()).unwrap();
            executor.parser_barrier = Some(worker_barrier);
            executor.execute(request("http://127.0.0.1/v1/embeddings".into())).await
        });
        tokio::time::timeout(Duration::from_secs(5), barrier.entered.notified()).await.unwrap();
        cancel.cancel();
        tokio::task::yield_now().await;
        assert!(!job.is_finished(), "running parser must settle before explicit cancellation resolves");
        assert_eq!(slots.available_permits(), 0);
        assert!(budget.owned_bytes() > Budget::context_bytes());
        drop(release);
        assert!(matches!(job.await.unwrap(), Err(Failure::Cancelled)));
        assert_eq!(slots.available_permits(), 1);
        assert_eq!(budget.owned_bytes(), Budget::context_bytes());
    }
}

#[tokio::test]
async fn caller_drop_discards_completion_but_keeps_worker_lease_until_exit() {
    let (operation, budget, slots) = operation();
    let cancel = operation.cancel().clone();
    let barrier = ParseBarrier::new(PauseAt::AfterParse);
    let release = Unpause(barrier.clone());
    let worker_barrier = barrier.clone();
    let job = tokio::spawn(async move {
        let mut executor = Executor::new(operation, limits(), ORIGIN.into()).unwrap();
        executor.parser_barrier = Some(worker_barrier);
        executor.execute(request("http://127.0.0.1/v1/embeddings".into())).await
    });
    tokio::time::timeout(Duration::from_secs(5), barrier.entered.notified()).await.unwrap();
    job.abort();
    assert!(matches!(job.await, Err(error) if error.is_cancelled()));
    assert!(cancel.is_cancelled());
    assert_eq!(cancel.origin(), decode::CancelOrigin::Owner);
    assert_eq!(slots.available_permits(), 0);
    assert!(budget.owned_bytes() > Budget::context_bytes());
    drop(release);
    tokio::time::timeout(Duration::from_secs(5), async {
        while slots.available_permits() == 0 { tokio::task::yield_now().await; }
    }).await.unwrap();
    assert_eq!(budget.owned_bytes(), Budget::context_bytes());
}

#[test]
fn queued_parser_cancellation_removes_work_before_the_parser_starts() {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().max_blocking_threads(1).build().unwrap();
    runtime.block_on(async {
        let blocked = ParseBarrier::new(PauseAt::BeforeParse);
        let release = Unpause(blocked.clone());
        let worker_blocked = blocked.clone();
        let blocker = tokio::task::spawn_blocking(move || worker_blocked.pause(PauseAt::BeforeParse));
        blocked.entered.notified().await;
        let (operation, budget, slots) = operation();
        let cancel = operation.cancel().clone();
        let barrier = ParseBarrier::new(PauseAt::BeforeParse);
        let _parser_release = Unpause(barrier.clone());
        let mut executor = Executor::new(operation, limits(), ORIGIN.into()).unwrap();
        executor.parser_barrier = Some(barrier.clone());
        let mut job = Box::pin(executor.execute(request("http://127.0.0.1/v1/embeddings".into())));
        assert!(std::future::poll_fn(|cx| std::task::Poll::Ready(job.as_mut().poll(cx))).await.is_pending());
        barrier.submitted.notified().await;
        cancel.cancel();
        assert!(std::future::poll_fn(|cx| std::task::Poll::Ready(job.as_mut().poll(cx))).await.is_pending());
        assert_eq!(slots.available_permits(), 0);
        assert!(budget.owned_bytes() > Budget::context_bytes());
        drop(release);
        blocker.await.unwrap();
        assert!(matches!(tokio::time::timeout(Duration::from_secs(5), &mut job).await.unwrap(), Err(Failure::Cancelled)));
        drop(job);
        let mut entered = Box::pin(barrier.entered.notified());
        assert!(std::future::poll_fn(|cx| std::task::Poll::Ready(entered.as_mut().poll(cx))).await.is_pending());
        drop(executor);
        assert_eq!(slots.available_permits(), 1);
        assert_eq!(budget.owned_bytes(), Budget::context_bytes());
    });
}

#[tokio::test]
async fn small_url_and_header_caps_fail_before_parser_or_wire() {
    let (operation, _, _) = operation();
    let mut small = limits(); small.url_bytes = 3;
    let mut executor = Executor::new(operation, small, ORIGIN.into()).unwrap();
    assert!(matches!(executor.execute(request("http://127.0.0.1/".into())).await, Err(Failure::Limit("native-http-url-bytes"))));
    let (operation, _, _) = self::operation();
    let mut small = limits(); small.header_bytes = 3;
    assert!(matches!(Executor::new(operation, small, ORIGIN.into()), Err(Failure::Limit("native-http-header-bytes"))));
}

#[tokio::test]
async fn cancellation_during_body_read_releases_decoder_and_reply_storage() {
    let mut reply = Reply::plain(200, b"{"); reply.held = true;
    let server = Loopback::new(vec![reply]).await;
    let (operation, budget, slots) = operation();
    let cancel = operation.cancel().clone();
    let progress = Arc::new(Notify::new());
    let collector_progress = progress.clone();
    let url = format!("{}/v1/embeddings", server.url);
    let job = tokio::spawn(async move {
        let mut executor = Executor::new(operation, limits(), ORIGIN.into()).unwrap();
        executor.decoder.body_progress = Some(collector_progress);
        executor.execute(request(url)).await
    });
    tokio::time::timeout(Duration::from_secs(5), progress.notified()).await.unwrap();
    cancel.cancel();
    assert!(matches!(tokio::time::timeout(Duration::from_secs(5), job).await.unwrap().unwrap(), Err(Failure::Cancelled)));
    assert_eq!(slots.available_permits(), 1);
    assert_eq!(budget.owned_bytes(), Budget::context_bytes());
}

#[tokio::test]
async fn independent_encoded_decoded_and_root_limits_are_terminal() {
    let body = vec![b' '; 8192];
    for which in 0..4 {
        let mut small = limits();
        let reply = match which {
            0 => { small.decode.max_encoded_bytes = 8; Reply::plain(200, &body) }
            1 => { small.decode.max_layer_decoded_bytes = 8; Reply::encoded(decode::tests::encode(decode::Coding::Gzip, &body), "gzip") }
            2 => { small.response_bytes = 8; Reply::plain(200, &body) }
            _ => { small.decode_owned = Budget::context_bytes() + 8; Reply::plain(200, &body) }
        };
        let mut server = Loopback::new(vec![reply]).await;
        let (operation, budget, slots) = operation();
        let mut executor = Executor::new(operation, small, ORIGIN.into()).unwrap();
        assert!(matches!(executor.execute(request(format!("{}/v1/embeddings", server.url))).await, Err(Failure::Limit(_))), "limit {which}");
        assert_eq!(executor.phase, Phase::Terminal);
        assert!(executor.decoder.pipeline.is_none());
        server.captured().await;
        drop(executor);
        assert_eq!(slots.available_permits(), 1);
        assert_eq!(budget.owned_bytes(), Budget::context_bytes());
    }
}

#[test]
fn wrong_owner_and_old_generation_never_reach_resume_callback() {
    let (operation, _, _) = operation();
    let mut executor = Executor::new(operation.clone(), limits(), ORIGIN.into()).unwrap();
    executor.phase = Phase::Response(2);
    for wrong_owner in [false, true] {
        let other = if wrong_owner { self::operation().0 } else { operation.clone() };
        let buffer = Buffer::new(&other, 8, 1).unwrap();
        let (body, charge, extra, operation) = buffer.into_parts();
        let reply = ReplyEnvelope { status: 200, body: Some(body), generation: if wrong_owner { 2 } else { 1 }, operation, _charge: charge, _extra: extra };
        let mut called = false;
        assert!(matches!(executor.resume(reply, |_| called = true), Err(Failure::Invalid("native-http-reply-owner"))));
        assert!(!called);
    }
}

#[derive(Default)]
struct Settings { model: Option<String>, key: Option<String> }
fn capture(settings: Arc<Mutex<Settings>>) -> impl Fn(&Budget) -> Result<CapturedConfig, Failure> + Send + 'static {
    move |budget| {
        let settings = settings.lock().unwrap();
        let model = settings.model.as_ref().map(|value| Text::from_str(value, budget)).transpose()?;
        let key = settings.key.as_ref().map(|value| Text::from_str(value, budget)).transpose()?;
        CapturedConfig::capture(model.as_ref(), key.as_ref(), None, None, budget)
    }
}

#[derive(Clone, Default)]
struct SyntheticCache(Arc<Mutex<BTreeMap<[u8; 32], Vec<u32>>>>);
impl ImmediateCache for SyntheticCache {
    fn read_batch(&mut self, keys: &[CacheKey], budget: &Budget) -> Result<OwnedVec<Option<CacheRow>>, CacheError> {
        let mut result = OwnedVec::new(budget).map_err(|error| CacheError::Terminal(error.into()))?;
        let rows = self.0.lock().unwrap();
        for key in keys {
            let row = if let Some(bits) = rows.get(&key.0) {
                let mut owned = OwnedVec::new(budget).map_err(|error| CacheError::Terminal(error.into()))?;
                owned.extend(bits).map_err(|error| CacheError::Terminal(error.into()))?;
                Some(CacheRow { key: *key, dimensions: bits.len(), bits: owned })
            } else { None };
            result.push(row).map_err(|error| CacheError::Terminal(error.into()))?;
        }
        Ok(result)
    }
    fn write_batch(&mut self, _: &EmbeddingTarget, entries: &[CacheWrite], budget: &Budget) -> Result<(), CacheError> {
        let mut rows = self.0.lock().unwrap();
        for entry in entries {
            budget.work(1).map_err(|error| CacheError::Terminal(error.into()))?;
            if let Some(key) = entry.key { rows.insert(key.0, entry.bits.as_slice().to_vec()); }
        }
        Ok(())
    }
}

fn retrieval(url: &str, cache: SyntheticCache, settings: Arc<Mutex<Settings>>, operation: Operation)
    -> NativeRetrieval<HypaV1Retrieval<SyntheticCache, Adapter<impl Fn(&Budget) -> Result<CapturedConfig, Failure> + Send + 'static>>>
{
    let budget = operation.budget();
    let target = EmbeddingTarget::capture(&Text::from_str("custom", budget).unwrap(), None,
        &Text::from_str(url, budget).unwrap(), &Text::from_str("old-cache-identity", budget).unwrap(), budget).unwrap();
    let diagnostic = SharedFailure::new(&operation).unwrap();
    let candidates = CandidateCount::new(&operation).unwrap();
    let adapter = Adapter::new(capture(settings), diagnostic.clone(), candidates.clone());
    let retrieval = HypaV1Retrieval::new(target, cache, adapter, 7).unwrap();
    NativeRetrieval::new(retrieval, Executor::new(operation, limits(), ORIGIN.into()).unwrap(), diagnostic, candidates)
}

fn vectors(count: usize) -> Vec<u8> {
    serde_json::to_vec(&json!({"data": (0..count).map(|_| json!({"embedding": [1.00000001, 2]})).collect::<Vec<_>>()})).unwrap()
}

#[tokio::test]
async fn real_hypa_batches_capture_current_settings_and_keep_f64_until_cache_roundtrip() {
    let mut first = Reply::plain(200, &vectors(50)); first.held_headers = true;
    let mut server = Loopback::new(vec![first, Reply::plain(200, &vectors(1)), Reply::plain(200, &vectors(1)), Reply::plain(200, &vectors(1))]).await;
    let settings = Arc::new(Mutex::new(Settings { model: Some("first-model".into()), key: None }));
    let cache = SyntheticCache::default();
    let url = format!("{}/v1", server.url);
    let (operation, budget, slots) = operation();
    let mut owner = retrieval(&url, cache.clone(), settings.clone(), operation);
    let documents = (0..51).map(|n| Text::from_str(&format!("synthetic document {n}"), &budget).unwrap()).collect();
    let mut prepare = Box::pin(owner.prepare(documents));
    let first = tokio::select! {
        request = server.captured() => request,
        _ = &mut prepare => panic!("held response completed early"),
    };
    *settings.lock().unwrap() = Settings { model: Some("second-model".into()), key: Some("second-key".into()) };
    server.release.notify_one();
    let prepared = prepare.await;
    assert!(prepared.value.is_ok(), "prepare failed: {:?}; diagnostic: {:?}", prepared.value.as_ref().err(), prepared.diagnostic);
    assert!(matches!(prepared.value.as_ref().unwrap().output, RetrievalOutput::Prepared));
    drop(prepared);
    let second = server.captured().await;
    let first_json: Value = serde_json::from_slice(&first.body).unwrap();
    let second_json: Value = serde_json::from_slice(&second.body).unwrap();
    assert_eq!(first_json["input"].as_array().unwrap().len(), 50);
    assert_eq!(second_json["input"].as_array().unwrap().len(), 1);
    assert_eq!(first_json["model"], "first-model");
    assert_eq!(second_json["model"], "second-model");
    assert!(!first.headers.contains_key("authorization"));
    assert_eq!(second.headers["authorization"], ["Bearer second-key"]);
    let query = owner.query(Text::from_str("synthetic query", &budget).unwrap()).await;
    let RetrievalOutput::Scored(rows) = &query.value.as_ref().unwrap().output else { panic!() };
    assert_eq!(rows.len(), 51);
    assert!(rows.iter().all(|row| row.score.to_bits() == (1.00000001f64 * 1.00000001 + 4.0).to_bits()));
    let query_wire: Value = serde_json::from_slice(&server.captured().await.body).unwrap();
    assert!(query_wire["input"].is_string());
    drop(owner);
    assert_eq!(slots.available_permits(), 0, "result retains admission and its outer vector lease");
    drop(query);
    assert_eq!(slots.available_permits(), 1);
    assert_eq!(budget.owned_bytes(), Budget::context_bytes());
    assert_eq!(cache.0.lock().unwrap().len(), 51);

    let (operation, budget, _) = self::operation();
    let mut cached = retrieval(&url, cache, settings, operation);
    let prepared = cached.prepare((0..51).map(|n| Text::from_str(&format!("synthetic document {n}"), &budget).unwrap()).collect()).await;
    assert!(prepared.value.is_ok()); drop(prepared);
    let query = cached.query(Text::from_str("synthetic query", &budget).unwrap()).await;
    let RetrievalOutput::Scored(rows) = &query.value.as_ref().unwrap().output else { panic!() };
    assert!(rows.iter().all(|row| row.score.to_bits() == (1.00000001f64 + 4.0).to_bits()));
    let wire: Value = serde_json::from_slice(&server.captured().await.body).unwrap();
    assert!(wire["input"].is_string(), "all document vectors must come from the synthetic cache");
}

#[tokio::test]
async fn later_batch_failure_retains_whole_error_json_and_writes_no_cache_rows() {
    let error = br#"{"error":"synthetic","details":{"code":17},"extra":[1,null]}"#;
    let mut server = Loopback::new(vec![Reply::plain(200, &vectors(50)), Reply::plain(429, error)]).await;
    let cache = SyntheticCache::default();
    let (operation, budget, slots) = operation();
    let mut owner = retrieval(&format!("{}/v1", server.url), cache.clone(), Arc::new(Mutex::new(Settings::default())), operation);
    let output = owner.prepare((0..51).map(|n| Text::from_str(&format!("document {n}"), &budget).unwrap()).collect()).await;
    assert!(output.value.is_err());
    let Some(Failure::Remote { status, diagnostic }) = &output.diagnostic else { panic!("owned remote diagnostic missing") };
    assert_eq!(*status, 429);
    assert_eq!(diagnostic.as_unicode().unwrap().as_bytes(), error);
    assert!(cache.0.lock().unwrap().is_empty());
    server.captured().await; server.captured().await;
    drop(owner);
    assert_eq!(slots.available_permits(), 0);
    assert!(budget.owned_bytes() > Budget::context_bytes());
    drop(output);
    assert_eq!(slots.available_permits(), 1);
    assert_eq!(budget.owned_bytes(), Budget::context_bytes());
}

#[tokio::test]
async fn native_gzip_emits_written_output_before_same_call_footer_error() {
    let mut gzip = decode::tests::encode(decode::Coding::Gzip, JSON);
    let crc = gzip.len() - 8; gzip[crc] ^= 1;
    let mut baseline = async_compression::tokio::bufread::GzipDecoder::new(std::io::Cursor::new(gzip.clone()));
    let mut bytes = [0u8; 1024];
    let written = baseline.read(&mut bytes).await.unwrap();
    assert_eq!(&bytes[..written], JSON);
    assert!(baseline.read(&mut bytes).await.is_err());
    let cancel = Cancellation::new();
    let budget = decode::tests::budget(&cancel);
    let source = decode::tests::Scripted::new(vec![decode::tests::Action::Data(gzip)]);
    let mut pipeline = Pipeline::native_plugin(Box::new(source), true, decode::tests::limits(), budget.clone()).unwrap();
    let part = pipeline.pull(&cancel).await.unwrap().unwrap();
    assert_eq!(part.bytes(), JSON);
    assert!(matches!(pipeline.pull(&cancel).await, Err(DecodeError::Codec { .. })));
    drop(part); drop(pipeline);
    assert_eq!(budget.owned_bytes(), Budget::context_bytes());
}

#[tokio::test]
async fn decoder_work_is_cumulative_and_private_chunks_do_not_escape_reservation() {
    let mut server = Loopback::new(vec![Reply::plain(200, JSON); 64]).await;
    let (operation, budget, slots) = operation();
    let mut small = limits();
    // Identity intake charges 2N byte work, three units per nonempty frame, and two for EOF.
    // With 1..=N frames, each reply costs 2N+5..=5N+2, so two fit and five cannot fit.
    small.decode_work = 2 * (5 * JSON.len() + 2);
    let mut executor = Executor::new(operation, small, ORIGIN.into()).unwrap();
    let mut successes = 0;
    for _ in 0..64 {
        let result = executor.execute(request(format!("{}/v1/embeddings", server.url))).await;
        server.captured().await;
        match result {
            Ok(reply) => { executor.resume(reply, |reply| drop(reply)).unwrap(); successes += 1; }
            Err(Failure::Limit("work")) => break,
            other => panic!("unexpected cumulative decoder result: {}", other.is_ok()),
        }
        assert_eq!(executor.decoder.budget.as_ref().unwrap().owned_bytes(), Budget::context_bytes());
    }
    assert!((2..=4).contains(&successes), "cumulative decoder budget allowed {successes} replies");
    assert_eq!(executor.phase, Phase::Terminal);
    assert!(budget.owned_bytes() >= small.decode_owned + Budget::context_bytes());
    drop(executor);
    assert_eq!(slots.available_permits(), 1);
    assert_eq!(budget.owned_bytes(), Budget::context_bytes());
}

#[tokio::test]
async fn raw_reply_lease_is_retained_through_real_provider_resume() {
    let mut server = Loopback::new(vec![Reply::plain(200, JSON)]).await;
    let (operation, budget, _) = operation();
    let mut executor = Executor::new(operation, limits(), ORIGIN.into()).unwrap();
    let mut service = Service::new();
    let target = RemoteTarget::capture(&Text::from_str("custom", &budget).unwrap(), None,
        Some(&Text::from_str(&format!("{}/v1", server.url), &budget).unwrap()), None, &budget).unwrap();
    let config = CapturedConfig::capture(None, None, None, None, &budget).unwrap();
    let Step::NeedsHost(HostRequest::Http(http)) = service.run(embeddings::Request {
        target, config, purpose: Purpose::Query, input: Input::Scalar(Text::from_str("synthetic", &budget).unwrap())
    }, &budget).unwrap() else { panic!() };
    let reply = executor.execute(http).await.unwrap();
    let capacity = reply.body.as_ref().unwrap().capacity();
    let headroom = budget.reserve_owned(budget.limits().max_owned_bytes - budget.owned_bytes() - capacity + 1).unwrap();
    let result = executor.resume(reply, |reply| service.resume(reply, &budget)).unwrap();
    assert!(matches!(result, Err(Failure::Limit("owned-bytes"))));
    server.captured().await;
    drop(headroom); drop(service); drop(executor);
    assert_eq!(budget.owned_bytes(), Budget::context_bytes());
}

#[tokio::test]
async fn native_redirects_preserve_baseline_method_and_replay_rules() {
    for status in [301, 302, 303, 307, 308] {
        let mut redirect = Reply::plain(status, b"");
        redirect.headers.push(("Location".into(), b"/destination?q=synthetic".to_vec()));
        let mut server = Loopback::new(vec![redirect.clone(), Reply::plain(200, JSON), redirect, Reply::plain(200, JSON)]).await;
        let url = format!("{}/start", server.url);
        let input = request(url.clone());
        let mut headers = input.headers.clone(); headers.push(("Origin".into(), ORIGIN.into()));
        let (operation, _, _) = operation();
        let baseline = native_plugin(url.clone(), input.body.clone(), headers, operation).await.unwrap();
        let first = server.captured().await; let redirected = server.captured().await;
        let actual = native_owner(url, limits()).await.unwrap();
        assert_eq!(actual, baseline);
        assert_eq!(server.captured().await, first);
        assert_eq!(server.captured().await, redirected);
        assert_eq!(redirected.target, "/destination?q=synthetic");
        assert_eq!(redirected.method, if status < 307 { "GET" } else { "POST" });
        assert_eq!(redirected.body, if status < 307 { Vec::new() } else { input.body });
    }
}

#[tokio::test]
async fn cancellation_while_headers_are_held_never_produces_a_reply() {
    let mut reply = Reply::plain(200, JSON); reply.held_headers = true;
    let mut server = Loopback::new(vec![reply]).await;
    let (operation, budget, slots) = operation();
    let cancel = operation.cancel().clone();
    let url = format!("{}/v1/embeddings", server.url);
    let job = tokio::spawn(async move {
        let mut executor = Executor::new(operation, limits(), ORIGIN.into()).unwrap();
        executor.execute(request(url)).await
    });
    server.captured().await;
    cancel.cancel(); server.release.notify_one();
    assert!(matches!(tokio::time::timeout(Duration::from_secs(5), job).await.unwrap().unwrap(), Err(Failure::Cancelled)));
    assert_eq!(slots.available_permits(), 1);
    assert_eq!(budget.owned_bytes(), Budget::context_bytes());
}

#[tokio::test]
async fn ready_cancellation_and_duplicate_dispatch_do_not_resume_or_send() {
    let (operation, _, _) = operation();
    let mut executor = Executor::new(operation.clone(), limits(), ORIGIN.into()).unwrap();
    executor.phase = Phase::Response(1);
    assert!(matches!(executor.execute(request("http://127.0.0.1/".into())).await, Err(Failure::Invalid("native-http-dispatch-state"))));
    let buffer = Buffer::new(&operation, 8, 1).unwrap();
    let (body, charge, extra, operation) = buffer.into_parts();
    let reply = ReplyEnvelope { status: 200, body: Some(body), generation: 1, operation, _charge: charge, _extra: extra };
    executor.operation.cancel().cancel();
    let mut called = false;
    assert!(matches!(executor.resume(reply, |_| called = true), Err(Failure::Cancelled)));
    assert!(!called);
}

#[tokio::test]
async fn completed_document_cache_survives_query_http_json_and_transport_failures() {
    for kind in 0..3 {
        let query_reply = if kind == 0 { Reply::plain(500, br#"{"error":"synthetic"}"#) }
            else if kind == 1 { Reply::plain(200, b"not json") }
            else { let mut reply = Reply::plain(200, b"{"); reply.held = true; reply };
        let mut server = Loopback::new(vec![Reply::plain(200, &vectors(1)), query_reply]).await;
        let cache = SyntheticCache::default();
        let (operation, budget, slots) = operation();
        let mut owner = retrieval(&format!("{}/v1", server.url), cache.clone(), Arc::new(Mutex::new(Settings::default())), operation);
        let prepared = owner.prepare(vec![Text::from_str("synthetic document", &budget).unwrap()]).await;
        assert!(prepared.value.is_ok()); drop(prepared);
        server.captured().await;
        assert_eq!(cache.0.lock().unwrap().len(), 1);
        let output = if kind == 2 {
            let mut query = Box::pin(owner.query(Text::from_str("query", &budget).unwrap()));
            tokio::select! { _ = server.captured() => {}, _ = &mut query => panic!("held body completed early") }
            server.worker.abort();
            query.await
        } else {
            let output = owner.query(Text::from_str("query", &budget).unwrap()).await;
            server.captured().await;
            output
        };
        assert!(output.value.is_err());
        assert_eq!(cache.0.lock().unwrap().len(), 1);
        if kind == 0 { assert!(matches!(output.diagnostic, Some(Failure::Remote { status: 500, .. }))); }
        if kind == 1 { assert!(matches!(output.diagnostic, Some(Failure::Decode { .. }))); }
        if kind == 2 { assert!(matches!(output.diagnostic, Some(Failure::Transport { .. }))); }
        drop(owner); drop(output);
        assert_eq!(slots.available_permits(), 1);
        assert_eq!(budget.owned_bytes(), Budget::context_bytes());
    }
}

#[tokio::test]
async fn surplus_provider_rows_and_retained_queries_have_independent_precompletion_leases() {
    let mut server = Loopback::new(vec![Reply::plain(200, &vectors(3)), Reply::plain(200, &vectors(1)), Reply::plain(200, &vectors(1))]).await;
    let (operation, budget, slots) = operation();
    let mut owner = retrieval(&format!("{}/v1", server.url), SyntheticCache::default(), Arc::new(Mutex::new(Settings::default())), operation);
    let prepared = owner.prepare(vec![Text::from_str("one document", &budget).unwrap()]).await;
    assert!(prepared.value.is_ok()); drop(prepared); server.captured().await;
    assert_eq!(owner.candidates.rows.load(Ordering::Relaxed), 4);
    let first = owner.query(Text::from_str("first query", &budget).unwrap()).await;
    server.captured().await;
    let before = budget.owned_bytes();
    let second = owner.query(Text::from_str("second query", &budget).unwrap()).await;
    server.captured().await;
    let per_query = 4 * size_of::<risunest_memory::hanurai::ScoredDocument>();
    assert!(budget.owned_bytes() >= before + per_query);
    for result in [&first, &second] {
        let RetrievalOutput::Scored(rows) = &result.value.as_ref().unwrap().output else { panic!() };
        assert_eq!(rows.len(), 3);
        assert_eq!(rows.iter().filter(|row| row.document.is_none()).count(), 2);
    }
    drop(owner); drop(first);
    assert_eq!(slots.available_permits(), 0);
    drop(second);
    assert_eq!(slots.available_permits(), 1);
    assert_eq!(budget.owned_bytes(), Budget::context_bytes());
}

#[tokio::test]
async fn result_admission_refuses_before_query_http_even_with_surplus_document_vectors() {
    let mut server = Loopback::new(vec![Reply::plain(200, &vectors(512)), Reply::plain(200, &vectors(1))]).await;
    let (operation, budget, _) = operation();
    let mut owner = retrieval(&format!("{}/v1", server.url), SyntheticCache::default(), Arc::new(Mutex::new(Settings::default())), operation);
    let prepared = owner.prepare(vec![Text::from_str("one document", &budget).unwrap()]).await;
    assert!(prepared.value.is_ok()); drop(prepared); server.captured().await;
    let query = Text::from_str("query", &budget).unwrap();
    let needed = 513 * size_of::<risunest_memory::hanurai::ScoredDocument>();
    let held = budget.reserve_owned(budget.limits().max_owned_bytes - budget.owned_bytes() - needed + 1).unwrap();
    let output = owner.query(query).await;
    assert!(matches!(output.value, Err(StageError::Limit(_))));
    assert!(matches!(server.requests.try_recv(), Err(mpsc::error::TryRecvError::Empty)));
    drop(held); drop(output); drop(owner);
    assert_eq!(budget.owned_bytes(), Budget::context_bytes());
}

#[test]
fn admission_does_not_queue_and_candidate_count_overflow_is_terminal() {
    let (operation, budget, slots) = operation();
    assert!(matches!(Operation::admit(slots.clone(), budget.clone(), operation.cancel().clone()), Err(Failure::Limit("native-http-active-operations"))));
    let candidates = CandidateCount::new(&operation).unwrap();
    candidates.rows.store(usize::MAX, Ordering::Relaxed);
    assert!(matches!(candidates.record(1), Err(Failure::Limit("native-http-candidate-count"))));
    assert!(matches!(candidates.reserve_result(), Err(Failure::Limit("native-http-result-bytes"))));
    drop(candidates); drop(operation);
    assert_eq!(slots.available_permits(), 1);
    assert_eq!(budget.owned_bytes(), Budget::context_bytes());
}

fn fixture_units(value: &Value) -> Vec<u16> {
    if let Some(text) = value.as_str() { return text.encode_utf16().collect(); }
    value.get("u").unwrap_or(value).as_array().expect("synthetic UTF16 units").iter()
        .map(|unit| u16::try_from(unit.as_u64().unwrap()).unwrap()).collect()
}
fn captured_text(value: &Value, budget: &Budget) -> Option<Text> {
    if value.is_null() || value.get("undefined") == Some(&Value::Bool(true)) { return None; }
    Some(Text::from_units(&fixture_units(value), budget).unwrap())
}

#[tokio::test]
async fn actual_source_intake_preserves_raw_service_and_native_url_observations() {
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../tests/fixtures/rust-core/providers/embeddings/native-http");
    let inputs: Value = serde_json::from_slice(&std::fs::read(directory.join("intake.cases.json")).unwrap()).unwrap();
    let observations = std::fs::read(directory.join("intake.expected.json"))
        .expect("Generate the reviewed actual-source intake observations in the coordinator's execution slot first");
    let observations: Value = serde_json::from_slice(&observations).unwrap();
    assert_eq!(inputs["schema"], 1); assert_eq!(observations["schema"], 1);
    let inputs = inputs["cases"].as_array().unwrap();
    let observed = observations["cases"].as_array().unwrap();
    assert_eq!(inputs.len(), observed.len());
    assert_eq!(inputs.len(), 46);
    let mut call_count = 0;
    let mut before_native_count = 0;
    for (input, expected) in inputs.iter().zip(observed) {
        assert_eq!(input["id"], expected["id"]);
        assert_eq!(expected["result"]["boundary"], "node-installed-plugin-ipc-observation");
        let calls = input["input"]["calls"].as_array().unwrap();
        let expected_calls = expected["result"]["calls"].as_array().unwrap();
        assert_eq!(calls.len(), expected_calls.len());
        for (call, expected) in calls.iter().zip(expected_calls) {
            call_count += 1;
            let (operation, budget, _) = operation();
            let captured = &expected["captured"];
            let model = captured_text(&captured["model"], &budget).unwrap();
            let endpoint = captured_text(&captured["effectiveUrl"], &budget);
            let target = RemoteTarget::capture(&model, None, endpoint.as_ref(), None, &budget).unwrap();
            let custom_model = captured_text(&captured["customModel"], &budget);
            let custom_key = captured_text(&captured["customKey"], &budget);
            let override_key = captured_text(&captured["oaikey"], &budget);
            let fallback_key = captured_text(&captured["supaKey"], &budget);
            let config = CapturedConfig::capture(custom_model.as_ref(), custom_key.as_ref(), override_key.as_ref(), fallback_key.as_ref(), &budget).unwrap();
            let embedding_input = if call["input"]["shape"] == "scalar" {
                Input::Scalar(Text::from_units(&fixture_units(&call["input"]["text"]), &budget).unwrap())
            } else {
                let mut texts = OwnedVec::new(&budget).unwrap();
                for text in call["input"]["texts"].as_array().unwrap() { texts.push(Text::from_units(&fixture_units(text), &budget).unwrap()).unwrap(); }
                Input::Batch(texts)
            };
            let purpose = if call["purpose"] == "query" { Purpose::Query } else { Purpose::Document };
            let mut service = Service::new();
            let Step::NeedsHost(HostRequest::Http(http)) = service.run(embeddings::Request { target, config, purpose, input: embedding_input }, &budget).unwrap() else { panic!() };
            assert_eq!(expected["rawService"].as_array().unwrap().len(), 1);
            let raw = &expected["rawService"][0];
            assert_eq!(http.url, String::from_utf16_lossy(&fixture_units(&raw["url"])), "{} raw URL", input["id"]);
            assert_eq!(http.body, String::from_utf16_lossy(&fixture_units(&raw["bodyText"])).as_bytes(), "{} body", input["id"]);
            let raw_headers: Vec<(String, String)> = raw["headers"].as_array().unwrap().iter().map(|pair| (
                String::from_utf16_lossy(&fixture_units(&pair[0])), String::from_utf16_lossy(&fixture_units(&pair[1]))
            )).collect();
            let native_inputs = expected["nativeFetchInput"].as_array().unwrap();
            if native_inputs.is_empty() {
                before_native_count += 1;
                assert_eq!(input["id"], "custom-invalid-url");
                assert_eq!(calls.len(), 1);
                assert!(raw_headers.is_empty());
                assert_eq!(http.headers, [("Content-Type".to_owned(), "application/json".to_owned())]);
                assert_eq!(expected["ipcRequest"], json!({"undefined": true}));
                assert_eq!(expected["ipcResponse"], json!({"undefined": true}));
                assert_eq!(expected["ipcEvents"], json!([]));
                assert_eq!(expected["jsResponse"], json!([]));
                assert_eq!(expected["globalFetch"].as_array().unwrap().len(), 1);
                assert_eq!(expected["globalFetch"][0]["ok"], false);
                assert_eq!(expected["globalFetch"][0]["status"], 400);
                assert!(expected["outcome"].get("failure").is_some());
                assert!(matches!(parse_url(http.url, limits().url_bytes, &operation, None).await, Err(Failure::Invalid("native-http-url"))));
                continue;
            }
            assert_eq!(native_inputs.len(), 1);
            let native_headers: Vec<(String, String)> = native_inputs[0]["headers"].as_array().unwrap().iter().map(|pair| (
                String::from_utf16_lossy(&fixture_units(&pair[0])), String::from_utf16_lossy(&fixture_units(&pair[1]))
            )).collect();
            assert!(raw_headers.iter().all(|header| native_headers.contains(header)), "{} raw header preservation", input["id"]);
            assert_eq!(http.headers, native_headers, "{} headers after globalFetch JSON augmentation", input["id"]);
            let header_intake: Result<Vec<(String, String)>, Failure> = http.headers.into_iter().map(|(name, value)| {
                let bytes = ownership::header_payload(value, &operation)?;
                let header = HeaderValue::from_maybe_shared(bytes).map_err(|_| Failure::Invalid("native-http-header-value"))?;
                Ok((name.to_ascii_lowercase(), std::str::from_utf8(header.as_bytes()).unwrap().to_owned()))
            }).collect();
            let parsed = parse_url(http.url, limits().url_bytes, &operation, None).await;
            if input["id"] == "data-url-hypa" {
                assert!(matches!(parsed, Err(Failure::Unsupported("native-http-experiment-scheme"))));
                assert!(expected["ipcRequest"].get("url").is_some(), "actual source reaches the plugin for the non-HTTP gap");
            } else if let Some(url) = expected["ipcRequest"].get("url") {
                let parsed = parsed.unwrap_or_else(|error| panic!("{} native intake: {error:?}", input["id"]));
                assert_eq!(parsed.url.as_str(), String::from_utf16_lossy(&fixture_units(url)), "{} native URL", input["id"]);
                let mut actual_headers = header_intake.unwrap(); actual_headers.sort();
                let mut expected_headers: Vec<(String, String)> = expected["ipcRequest"]["headers"].as_array().unwrap().iter().map(|pair| (
                    String::from_utf16_lossy(&fixture_units(&pair[0])), String::from_utf16_lossy(&fixture_units(&pair[1]))
                )).collect();
                expected_headers.sort();
                assert_eq!(actual_headers, expected_headers, "{} normalized headers", input["id"]);
            } else {
                assert!(parsed.is_err() || header_intake.is_err(), "{} must reject at the observed pre-IPC intake boundary", input["id"]);
            }
        }
    }
    assert_eq!((call_count, before_native_count), (47, 1));
}
