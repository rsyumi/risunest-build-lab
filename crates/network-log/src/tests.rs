use super::*;
use std::{fs, time::Duration};

struct Fixture {
    root: tempfile::TempDir,
    recorder: Recorder,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let recorder =
            Recorder::open(root.path().join("recent"), root.path().join("kept")).unwrap();
        Self { root, recorder }
    }
    fn enable(&self, retain: bool, hide: bool) {
        self.recorder
            .configure(Settings {
                enabled: true,
                retain,
                hide_auth: hide,
                ..Settings::default()
            })
            .unwrap();
    }
    fn pause_writer(&self) -> mpsc::Sender<()> {
        let (release, wait) = mpsc::channel();
        let (ready, started) = mpsc::channel();
        self.recorder.0.sender.send(Event::Job(Box::new(move |_| {
            ready.send(()).unwrap();
            let _ = wait.recv_timeout(Duration::from_secs(15));
        }))).unwrap();
        started.recv_timeout(Duration::from_secs(15)).unwrap();
        release
    }
    fn begin(&self) -> CaptureGuard {
        self.recorder
            .begin(
                "synthetic",
                "POST",
                "https://synthetic.example/path?key=query-remains",
                &http::HeaderMap::new(),
            )
            .guard()
    }
    fn detail(&self, capture: &Capture) -> Record {
        self.recorder
            .record(capture.0.as_ref().unwrap().id.clone())
            .unwrap()
    }
    fn snapshot(&self, capture: &Capture) -> Snapshot {
        self.recorder
            .snapshot(capture.0.as_ref().unwrap().id.clone())
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.recorder.shutdown();
    }
}
fn text_headers() -> http::HeaderMap {
    let mut h = http::HeaderMap::new();
    h.insert("content-type", "text/event-stream".parse().unwrap());
    h
}

#[test]
fn defaults_are_opt_in_and_disabled_capture_does_not_copy_or_enqueue() {
    let f = Fixture::new();
    assert!(!f.recorder.settings().enabled);
    assert!(!f.recorder.settings().capture_binary);
    assert!(f.recorder.settings().hide_auth);
    assert_eq!(f.recorder.settings().buffer_mib, 32);
    let capture = f.begin();
    capture.append(Side::Response, &[7; 65536]);
    capture.finish("complete");
    assert!(capture.0 .0.is_none());
    assert_eq!(f.recorder.0.queued.load(Ordering::Relaxed), 0);
    assert!(f.recorder.list(Query::default()).unwrap().items.is_empty());
}
#[test]
fn finite_auth_masking_preserves_duplicates_and_does_not_scrub_other_content() {
    let f = Fixture::new();
    f.enable(false, true);
    let secret = "sk_synthetic_only_abcdefghijklmnopqrstuvwxyz7vQ9m2";
    let mut headers = text_headers();
    headers.append("authorization", format!("Bearer {secret}").parse().unwrap());
    headers.append(
        "authorization",
        "Basic synthetic-credentials".parse().unwrap(),
    );
    headers.insert("x-unknown-key", "unchanged-unknown-secret".parse().unwrap());
    headers.insert("cookie", "session=synthetic".parse().unwrap());
    let c = f
        .recorder
        .begin(
            "test",
            "POST",
            "https://synthetic.example/?token=visible",
            &headers,
        )
        .guard();
    c.append(Side::Request, b"body-secret-remains");
    c.response(401, "https://synthetic.example/?token=visible", &headers);
    c.append(Side::Response, b"error-body-remains");
    c.finish("complete");
    let record = f.detail(&c);
    assert_eq!(
        record
            .request
            .headers
            .iter()
            .filter(|h| h.name == "authorization")
            .count(),
        2
    );
    assert!(record
        .request
        .headers
        .iter()
        .any(|h| h.value == "Bearer sk••••••7vQ9m2"));
    assert!(record
        .request
        .headers
        .iter()
        .any(|h| h.value == "Basic [REDACTED]"));
    let mut output = Vec::new();
    f.snapshot(&c).export_json(&mut output).unwrap();
    let json: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["request"]["body"]["data"], "body-secret-remains");
    assert_eq!(json["response"]["body"]["data"], "error-body-remains");
    assert!(!String::from_utf8(output).unwrap().contains(secret));
    f.enable(false, false);
    assert_eq!(
        f.detail(&c).request.headers[0].value,
        record.request.headers[0].value
    );
    let unhidden = f
        .recorder
        .begin("test", "GET", "https://synthetic.example", &headers)
        .guard();
    unhidden.finish("complete");
    assert!(f
        .detail(&unhidden)
        .request
        .headers
        .iter()
        .any(|h| h.value == format!("Bearer {secret}")));
}
#[test]
fn known_header_rules_are_deterministic_and_short_or_structured_tokens_are_hidden() {
    for name in ["X-Api-Key", "api-key", "X-Goog-API-KEY"] {
        assert_eq!(masking::mask(name, b"short"), Some("[REDACTED]".into()));
        assert_eq!(
            masking::mask(name, b"abcdefghijklmnop"),
            Some("••••••mnop".into())
        );
        assert_eq!(
            masking::mask(name, b"abcdefghijklmnopqrstuvwxyz123456"),
            Some("ab••••••123456".into())
        );
        assert_eq!(
            masking::mask(name, b"ey.synthetic.jwt-token-more-than-32"),
            Some("[REDACTED]".into())
        );
        assert_eq!(
            masking::mask(name, b"ghp_synthetic-abcdefghijklmnopqrstuvwxyz"),
            Some("[REDACTED]".into())
        );
    }
    for name in ["Cookie", "Set-Cookie", "X-Amz-Security-Token"] {
        assert_eq!(
            masking::mask(name, b"abcdefghijklmnopqrstuvwxyz123456"),
            Some("[REDACTED]".into())
        );
    }
    assert_eq!(
        masking::mask("Proxy-Authorization", b"Token abcdefghijklmnop"),
        Some("Token ••••••mnop".into())
    );
    assert_eq!(
        masking::mask("Authorization", b"Digest key=value"),
        Some("Digest [REDACTED]".into())
    );
    assert_eq!(masking::mask("x-unknown", b"visible"), None);
}
#[test]
fn sse_raw_bytes_split_unicode_and_partial_tail_round_trip_through_json() {
    let f = Fixture::new();
    f.enable(false, false);
    let c = f.begin();
    c.response(200, "https://synthetic.example/final", &text_headers());
    let text = ": heartbeat\r\nevent: token\r\nid: 5\r\ndata: 한글😀\r\ndata: \"x\"\\\t\r\n\r\ndata: [DONE]\n\nunknown: preserve\rdata: unfinished";
    for bytes in text.as_bytes().chunks(7) {
        c.append(Side::Response, bytes);
    }
    c.finish("error");
    let mut snapshot = f.snapshot(&c);
    assert_eq!(snapshot.record.state, "error");
    assert_eq!(snapshot.body_page(Side::Response, 0).unwrap().text, text);
    let mut output = Vec::new();
    f.snapshot(&c).export_json(&mut output).unwrap();
    let json: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["response"]["body"]["data"], text);
    assert_eq!(json["stream"]["kind"], "sse");
    assert!(json["stream"]["observations"].as_array().unwrap().len() > 1);
}
#[test]
fn stopped_captures_do_not_resume_or_cancel_the_transport() {
    let f = Fixture::new();
    f.enable(false, false);
    let c = f.begin();
    c.append(Side::Response, b"before");
    f.recorder.configure(Settings::default()).unwrap();
    c.append(Side::Response, b"off");
    f.enable(false, false);
    c.append(Side::Response, b"after");
    let record = f.detail(&c);
    assert_eq!(record.state, "incomplete");
    assert_eq!(record.capture_error.as_deref(), Some("stopped"));
    assert_eq!(
        f.snapshot(&c).body_page(Side::Response, 0).unwrap().text,
        "before"
    );
}
#[test]
fn promotion_coordinates_active_writes_and_retention_is_snapshotted() {
    let f = Fixture::new();
    f.enable(false, false);
    let c = f.begin();
    c.append(Side::Response, b"first");
    f.enable(true, false);
    assert!(!f.detail(&c).retained);
    let id = f.detail(&c).id;
    f.recorder.retain(id.clone()).unwrap();
    c.append(Side::Response, b"second");
    c.finish("complete");
    assert!(f.detail(&c).retained);
    assert!(!f.root.path().join("recent").join(&id).exists());
    assert_eq!(
        f.snapshot(&c).body_page(Side::Response, 0).unwrap().text,
        "firstsecond"
    );
    let next = f.begin();
    next.finish("complete");
    assert!(f.detail(&next).retained);
}
#[test]
fn completed_recent_records_rotate_but_retained_and_active_records_survive() {
    let f = Fixture::new();
    f.enable(false, false);
    f.recorder
        .configure(Settings {
            enabled: true,
            recent_count: 100,
            ..Settings::default()
        })
        .unwrap();
    let kept = f.begin();
    kept.finish("complete");
    let kept_id = f.detail(&kept).id;
    f.recorder.retain(kept_id.clone()).unwrap();
    let active = f.begin();
    let active_id = f.detail(&active).id;
    let mut first = String::new();
    for index in 0..101 {
        let c = f.begin();
        c.finish("complete");
        let id = f.detail(&c).id;
        if index == 0 {
            first = id;
        }
    }
    assert!(f.recorder.record(first).is_err());
    assert!(f.recorder.record(active_id).is_ok());
    assert!(f.recorder.record(kept_id).unwrap().retained);
}
#[test]
fn disabled_binary_capture_keeps_metadata_and_text_but_no_binary_payload() {
    let f = Fixture::new();
    f.enable(false, false);
    let c = f.begin();
    let mut headers = http::HeaderMap::new();
    headers.insert("content-type", "application/octet-stream".parse().unwrap());
    c.response(200, "https://synthetic.example", &headers);
    f.detail(&c);
    let release = f.pause_writer();
    c.append(Side::Response, &vec![0xff; 100_000]);
    assert_eq!(f.recorder.0.queued.load(Ordering::Relaxed), 0);
    release.send(()).unwrap();
    c.append(Side::Request, b"still-recorded");
    c.finish("complete");
    let record = f.detail(&c);
    assert_eq!(record.response.body.captured_bytes, 0);
    assert_eq!(record.response.status, Some(200));
    assert_eq!(record.response.body.observed_bytes, 100_000);
    assert!(record.response.body.truncated);
    assert_eq!(record.capture_error.as_deref(), Some("binary-skipped"));
    assert!(!f.recorder.list(Query::default()).unwrap().items[0].incomplete);
    assert_eq!(
        f.snapshot(&c).body_page(Side::Request, 0).unwrap().text,
        "still-recorded"
    );
    let mut output = Vec::new();
    f.snapshot(&c).export_json(&mut output).unwrap();
    let json: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["response"]["body"]["encoding"], "hex");
}
#[test]
fn disk_failure_does_not_escape_capture_or_hide_the_incomplete_reason() {
    let f = Fixture::new();
    f.enable(false, false);
    let c = f.begin();
    let id = f.detail(&c).id;
    let path = f.root.path().join("recent").join(id).join("response");
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    c.append(Side::Response, b"not-written");
    c.finish("complete");
    let record = f.detail(&c);
    assert_eq!(record.state, "complete");
    assert_eq!(record.capture_error.as_deref(), Some("write-failed"));
    assert!(record.response.body.truncated);
}
#[test]
fn pages_do_not_split_utf8_and_export_escapes_across_64k_boundaries() {
    let f = Fixture::new();
    f.enable(false, false);
    let c = f.begin();
    let text = "x".repeat(PAGE_BYTES - 1) + "😀tail\"\\\n";
    for chunk in text.as_bytes().chunks(16384) {
        c.append(Side::Response, chunk);
    }
    c.finish("complete");
    let mut snapshot = f.snapshot(&c);
    let first = snapshot.body_page(Side::Response, 0).unwrap();
    assert_eq!(first.next_offset, (PAGE_BYTES - 1) as u64);
    let next = snapshot
        .body_page(Side::Response, first.next_offset)
        .unwrap();
    assert_eq!(first.text + &next.text, text);
    let mut output = Vec::new();
    f.snapshot(&c).export_json(&mut output).unwrap();
    let json: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["response"]["body"]["data"], text);
}
#[test]
fn restart_cleans_recent_records_and_keeps_retained_records() {
    let f = Fixture::new();
    f.enable(false, false);
    let temp = f.begin();
    temp.finish("complete");
    let temp_id = f.detail(&temp).id;
    f.enable(true, false);
    let kept = f.begin();
    kept.append(Side::Response, b"kept");
    kept.finish("complete");
    let id = f.detail(&kept).id;
    f.recorder.shutdown();
    let reopened =
        Recorder::open(f.root.path().join("recent"), f.root.path().join("kept")).unwrap();
    assert!(reopened.record(temp_id).is_err());
    assert!(reopened.record(id).unwrap().retained);
    reopened.shutdown();
}
#[test]
fn empty_websocket_control_messages_are_observable_and_event_pages_are_bounded() {
    let f = Fixture::new();
    f.enable(false, false);
    let c = f
        .recorder
        .begin(
            "ws",
            "WS",
            "wss://synthetic.example",
            &http::HeaderMap::new(),
        )
        .guard();
    for _ in 0..70 {
        c.append_message(Side::Response, b"", Some("close"));
    }
    c.finish("complete");
    let mut snapshot = f.snapshot(&c);
    let first = snapshot.observations(0).unwrap();
    assert_eq!(first.text.lines().count(), 50);
    let second = snapshot.observations(first.next_offset).unwrap();
    assert_eq!(second.text.lines().count(), 20);
}

#[test]
fn content_coding_left_by_the_transport_is_labeled_and_follows_binary_capture() {
    let zip = [31, 139, 8, 0, 1, 2, 3, 4];
    for capture_binary in [false, true] {
        let f = Fixture::new();
        f.recorder.configure(Settings { enabled: true, capture_binary, ..Settings::default() }).unwrap();
        let mut request = http::HeaderMap::new();
        request.insert("content-type", "application/json".parse().unwrap());
        request.insert("content-encoding", " BR ".parse().unwrap());
        let c = f.recorder.begin("synthetic", "POST", "https://synthetic.example", &request).guard();
        c.append(Side::Request, b"compressed-request");
        let mut response = text_headers();
        response.insert("content-encoding", "gzip".parse().unwrap());
        c.response(200, "https://synthetic.example", &response);
        c.append(Side::Response, &zip);
        c.finish("complete");
        let record = f.detail(&c);
        assert_eq!(record.request.body.representation, "encoded-br");
        assert_eq!(record.response.body.representation, "encoded-gzip");
        assert!(record.request.body.binary && record.response.body.binary);
        assert_eq!(record.response.status, Some(200));
        assert_eq!(record.response.body.observed_bytes, zip.len() as u64);
        let item = f.recorder.list(Query::default()).unwrap().items.remove(0);
        if capture_binary {
            assert!(record.capture_error.is_none());
            assert!(!item.incomplete);
            assert_eq!(f.snapshot(&c).body_page(Side::Response, 0).unwrap().text, "1f8b080001020304");
        } else {
            assert_eq!(record.capture_error.as_deref(), Some("binary-skipped"));
            assert_eq!(record.response.body.captured_bytes, 0);
            assert!(record.response.body.truncated);
            assert!(!item.incomplete);
        }
    }
    let f = Fixture::new();
    f.enable(false, false);
    let mut identity = text_headers();
    identity.insert("content-encoding", "identity".parse().unwrap());
    let c = f.recorder.begin("synthetic", "POST", "https://synthetic.example", &identity).guard();
    c.append(Side::Request, b"data: plain\n\n");
    c.finish("complete");
    let record = f.detail(&c);
    assert_eq!(record.request.body.representation, "observed");
    assert_eq!(f.snapshot(&c).copy_body(Side::Request).unwrap(), "data: plain\n\n");
}
#[test]
fn websocket_binary_export_is_lossless_and_text_messages_remain_readable() {
    let f = Fixture::new();
    f.recorder.configure(Settings { enabled: true, capture_binary: true, ..Settings::default() }).unwrap();
    let c = f
        .recorder
        .begin(
            "ws",
            "WS",
            "wss://synthetic.example",
            &http::HeaderMap::new(),
        )
        .guard();
    c.append_message(Side::Response, b"hello", Some("text"));
    c.append_message(Side::Response, &[0, 255, 128], Some("binary"));
    c.finish("complete");
    let page = f
        .snapshot(&c)
        .body_range(Side::Response, 0, 5, Some(false))
        .unwrap();
    assert_eq!(page.text, "hello");
    let mut json = Vec::new();
    f.snapshot(&c).export_json(&mut json).unwrap();
    let json: serde_json::Value = serde_json::from_slice(&json).unwrap();
    assert_eq!(json["response"]["body"]["encoding"], "hex");
    assert_eq!(json["response"]["body"]["data"], "68656c6c6f00ff80");
}
#[test]
fn readers_pin_files_and_clipboard_rejects_over_16_mib_without_loading_them() {
    let f = Fixture::new();
    f.enable(false, true);
    let c = f.begin();
    c.finish("complete");
    let id = f.detail(&c).id;
    let a = f.snapshot(&c);
    let b = f.snapshot(&c);
    let d = f.snapshot(&c);
    let e = f.snapshot(&c);
    assert!(f.recorder.snapshot(id.clone()).is_err());
    assert_eq!(
        f.recorder.delete(id.clone()),
        Err("network-log-readers-busy".into())
    );
    drop((a, b, d, e));
    let task_id = id.clone();
    f.recorder
        .job(move |store| {
            let mut record = store.record(&task_id)?;
            record.response.body.captured_bytes = 16 * MIB + 1;
            std::fs::OpenOptions::new()
                .write(true)
                .open(store.dir(&record).join("response"))
                .unwrap()
                .set_len(16 * MIB + 1)
                .unwrap();
            store.update(&record)
        })
        .unwrap();
    assert_eq!(
        f.snapshot(&c).copy_body(Side::Response),
        Err("network-log-clipboard-too-large".into())
    );
    f.recorder.delete(id).unwrap();
}
#[test]
fn quota_stops_capture_and_preserves_existing_retained_requests() {
    let f = Fixture::new();
    f.enable(true, true);
    let kept = f.begin();
    kept.append(Side::Response, b"preserved");
    kept.finish("complete");
    let id = f.detail(&kept).id;
    f.recorder
        .configure(Settings {
            enabled: true,
            retain: true,
            retained_mib: 128,
            ..Settings::default()
        })
        .unwrap();
    let c = f.begin();
    let task_id = f.detail(&c).id;
    f.recorder
        .job(move |store| {
            let mut record = store.record(&task_id)?;
            // A sparse synthetic file reaches the budget without allocating a giant fixture.
            let length = 128 * MIB - 3 * MIB;
            std::fs::OpenOptions::new()
                .write(true)
                .open(store.dir(&record).join("response"))
                .unwrap()
                .set_len(length)
                .unwrap();
            record.response.body.captured_bytes = length;
            store.bytes[1] += length;
            store.update(&record)
        })
        .unwrap();
    for _ in 0..32 {
        c.append(Side::Response, &[0; 65536]);
        let _ = f.detail(&c);
    }
    c.finish("complete");
    assert_eq!(f.detail(&c).capture_error.as_deref(), Some("quota"));
    assert_eq!(
        f.recorder
            .snapshot(id)
            .unwrap()
            .copy_body(Side::Response)
            .unwrap(),
        "preserved"
    );
    assert!(f.recorder.list(Query::default()).unwrap().retained_bytes <= 128 * MIB);
}

#[tokio::test(flavor = "current_thread")]
async fn real_http_keeps_request_body_redirect_url_encoding_and_cancellation() {
    use crate::reqwest::RequestBuilderExt;
    use std::io::Write;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let f = Fixture::new();
    f.enable(false, true);
    crate::install(f.recorder.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let redirect = format!("{base}/final");
    let expected_url = redirect.clone();
    let server = tokio::spawn(async move {
        for index in 0..4 {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 1024];
            loop {
                let size = socket.read(&mut buffer).await.unwrap();
                if size == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..size]);
                if let Some(end) = request.windows(4).position(|v| v == b"\r\n\r\n") {
                    let body_length = if index < 2 { 17 } else { 0 };
                    if request.len() >= end + 4 + body_length {
                        break;
                    }
                }
            }
            if index < 2 {
                assert!(request.ends_with(b"synthetic-payload"));
            }
            match index {
                0 => socket.write_all(format!("HTTP/1.1 307 Temporary Redirect\r\nLocation: {redirect}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap(),
                1 | 2 => {
                    let mut zip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default()); zip.write_all(b"data: readable\n\n").unwrap(); let zip = zip.finish().unwrap();
                    socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Encoding: gzip\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", zip.len()).as_bytes()).await.unwrap(); socket.write_all(&zip).await.unwrap();
                },
                _ => { socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n4\r\ndata\r\n").await.unwrap(); tokio::time::sleep(Duration::from_millis(100)).await; }
            }
        }
    });
    let client = ::reqwest::Client::builder().no_proxy().build().unwrap();
    let response = client
        .post(format!("{base}/redirect"))
        .body("synthetic-payload")
        .send_logged("adapter-auto-gzip")
        .await
        .unwrap();
    assert_eq!(response.url().as_str(), expected_url);
    assert_eq!(response.text().await.unwrap(), "data: readable\n\n");
    let client = ::reqwest::Client::builder()
        .no_proxy()
        .no_gzip()
        .build()
        .unwrap();
    let response = client
        .get(format!("{base}/compressed"))
        .send_logged("adapter-raw-gzip")
        .await
        .unwrap();
    assert_eq!(response.headers()["content-encoding"], "gzip");
    assert_eq!(&response.bytes().await.unwrap()[..2], &[31, 139]);
    let mut response = client
        .get(format!("{base}/cancel"))
        .send_logged("adapter-cancel")
        .await
        .unwrap();
    assert_eq!(response.chunk().await.unwrap().unwrap(), "data");
    drop(response);
    server.await.unwrap();
    let rows = f.recorder.list(Query::default()).unwrap();
    assert_eq!(rows.items.len(), 3);
    for row in rows.items {
        let record = f.recorder.record(row.id.clone()).unwrap();
        if record.source == "adapter-raw-gzip" {
            assert_eq!(record.response.body.representation, "encoded-gzip");
            assert!(record.response.body.binary);
            assert_eq!(record.capture_error.as_deref(), Some("binary-skipped"));
            continue;
        }
        let text = f
            .recorder
            .snapshot(row.id)
            .unwrap()
            .copy_body(Side::Response)
            .unwrap();
        if record.source == "adapter-cancel" {
            assert_eq!(record.state, "incomplete");
            assert_eq!(text, "data");
        } else {
            assert_eq!(record.state, "complete");
            assert_eq!(text, "data: readable\n\n");
        }
    }
}

#[test]
fn large_export_streams_escaped_body_bytes() {
    const BODY: u64 = 4 * MIB;
    struct Counter(u64);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 += bytes.len() as u64;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let f = Fixture::new();
    f.enable(false, true);
    let c = f.begin();
    c.finish("complete");
    let id = f.detail(&c).id;
    f.recorder
        .job(move |store| {
            let mut record = store.record(&id)?;
            record.response.body.captured_bytes = BODY;
            record.response.body.observed_bytes = BODY;
            std::fs::OpenOptions::new()
                .write(true)
                .open(store.dir(&record).join("response"))
                .unwrap()
                .set_len(BODY)
                .unwrap();
            store.bytes[0] += BODY;
            store.update(&record)
        })
        .unwrap();
    let mut output = Counter(0);
    f.snapshot(&c).export_json(&mut output).unwrap();
    // NUL is emitted as six JSON escape bytes, without retaining the output.
    assert!(output.0 >= BODY * 6 && output.0 < BODY * 6 + MIB);
}
#[test]
fn concurrent_streams_and_a_single_enormous_chunk_keep_the_shared_queue_bounded() {
    let f = Fixture::new();
    f.enable(false, true);
    let release = f.pause_writer();
    let captures: Vec<_> = (0..4).map(|_| f.begin()).collect();
    let huge = vec![b'x'; 64 * 1024 * 1024];
    let began = Instant::now();
    std::thread::scope(|scope| {
        for capture in &captures {
            let bytes = &huge;
            scope.spawn(move || {
                capture.append(Side::Response, bytes);
                capture.finish("complete");
            });
        }
    });
    // Overflow drops bytes instead of waiting for the paused writer.
    assert!(began.elapsed() < Duration::from_secs(2));
    assert!(f.recorder.0.queued.load(Ordering::Relaxed) <= f.recorder.0.queue_limit.load(Ordering::Relaxed));
    drop(huge);
    release.send(()).unwrap();
    for capture in captures {
        let record = f.detail(&capture);
        assert_eq!(record.state, "complete");
        assert_eq!(record.response.body.observed_bytes, 64 * MIB);
        assert_eq!(record.capture_error.as_deref(), Some("queue-full"));
        assert!(record.response.body.truncated);
    }
}

#[test]
fn interrupted_retained_record_recovers_only_committed_bytes_and_cleans_cache() {
    let f = Fixture::new();
    f.enable(true, true);
    let c = f.begin();
    c.append(Side::Response, b"kept");
    let record = f.detail(&c);
    let root = tempfile::tempdir().unwrap();
    let cache = root.path().join("recent");
    let kept = root.path().join("retained");
    {
        let mut store = storage::Store::open(cache.clone(), kept.clone()).unwrap();
        store.insert(&record, &Settings::default()).unwrap();
        fs::write(
            store.dir(&record).join("response"),
            b"kept-uncommitted-tail",
        )
        .unwrap();
        fs::write(cache.join("unflushed"), b"temporary").unwrap();
    }
    let recovered = Recorder::open(cache.clone(), kept.clone()).unwrap();
    let saved = recovered.record(record.id.clone()).unwrap();
    assert_eq!(saved.state, "incomplete");
    assert_eq!(saved.capture_error.as_deref(), Some("interrupted"));
    assert_eq!(
        recovered
            .snapshot(record.id.clone())
            .unwrap()
            .copy_body(Side::Response)
            .unwrap(),
        "kept"
    );
    assert_eq!(
        fs::metadata(kept.join(record.id).join("response"))
            .unwrap()
            .len(),
        4
    );
    assert!(!cache.join("unflushed").exists());
    recovered.shutdown();
}

#[test]
fn overflowing_request_keeps_response_metadata_and_response_body() {
    let f = Fixture::new();
    f.recorder.configure(Settings { enabled: true, buffer_mib: 4, ..Settings::default() }).unwrap();
    let c = f.begin();
    f.detail(&c);
    let release = f.pause_writer();
    c.append(Side::Request, &vec![b'x'; 6 * MIB as usize]);
    c.response(201, "https://synthetic.example/final", &text_headers());
    release.send(()).unwrap();
    let pending = f.detail(&c);
    assert_eq!(pending.response.status, Some(201));
    assert!(!pending.response.headers.is_empty());
    c.append(Side::Response, b"data: preserved\n\n");
    c.finish("complete");
    let record = f.detail(&c);
    assert_eq!(record.capture_error.as_deref(), Some("queue-full"));
    assert!(record.request.body.truncated);
    assert!(!record.response.body.truncated);
    assert_eq!(
        f.snapshot(&c).copy_body(Side::Response).unwrap(),
        "data: preserved\n\n"
    );
}

#[test]
fn idle_websocket_does_not_write_the_index_but_elapsed_time_advances() {
    let f = Fixture::new();
    f.enable(false, false);
    let c = f
        .recorder
        .begin(
            "ws",
            "WS",
            "wss://synthetic.example",
            &http::HeaderMap::new(),
        )
        .guard();
    c.response(101, "wss://synthetic.example", &http::HeaderMap::new());
    f.detail(&c);
    let index = f.root.path().join("recent/index.sqlite");
    let before = fs::metadata(&index).unwrap().modified().unwrap();
    std::thread::sleep(Duration::from_millis(700));
    assert_eq!(fs::metadata(&index).unwrap().modified().unwrap(), before);
    let record = f.detail(&c);
    assert!(record.elapsed_ms >= 700);
    assert_eq!(fs::metadata(&index).unwrap().modified().unwrap(), before);
    c.append_message(Side::Response, b"hello", Some("text"));
    assert_eq!(f.detail(&c).response.body.captured_bytes, 5);
    c.finish("complete");
    assert_eq!(f.detail(&c).state, "complete");
}

#[test]
fn sse_arrival_lookup_uses_the_chunk_containing_each_event_end() {
    use std::io::Read;
    let f = Fixture::new();
    f.enable(false, false);
    let c = f.begin();
    c.response(200, "https://synthetic.example", &text_headers());
    for index in 0..200 {
        c.append(Side::Response, &vec![b'x'; 10 + index]);
    }
    c.finish("complete");
    let mut snapshot = f.snapshot(&c);
    let mut text = String::new();
    (&mut snapshot.events)
        .take(snapshot.record.event_bytes)
        .read_to_string(&mut text)
        .unwrap();
    let mut lines = Vec::new();
    let mut position = 0;
    for line in text.split_inclusive('\n') {
        lines.push((position, serde_json::from_str::<Observation>(line).unwrap()));
        position += line.len() as u64;
    }
    assert_eq!(lines.len(), 200);
    // Two events end in every chunk, so each 50-event page ends inside a chunk that the
    // following page continues from.
    let ends = lines
        .iter()
        .flat_map(|(_, c)| [c.offset + 1, c.offset + c.length])
        .collect::<Vec<_>>();
    let mut from = 0;
    for (page, ends) in ends.chunks(49).enumerate() {
        let arrivals = snapshot.arrival_times(ends, from).unwrap();
        let first = page * 49;
        let expected = (first..first + ends.len())
            .map(|event| Some(lines[event / 2].1.elapsed_ms))
            .collect::<Vec<_>>();
        assert_eq!(arrivals.times, expected);
        assert_eq!(arrivals.next, lines[(first + ends.len() - 1) / 2].0);
        from = arrivals.next;
    }
    assert_eq!(
        snapshot
            .arrival_times(&[snapshot.record.response.body.captured_bytes + 1], from)
            .unwrap(),
        Arrivals { times: vec![None], next: from }
    );
    // A position past the chunk that holds an end cannot report its time.
    let skipped = snapshot.arrival_times(&[lines[0].1.offset + 1], lines[1].0).unwrap();
    assert_eq!(skipped.times, vec![None]);
    assert!(snapshot.arrival_times(&[1], lines[1].0 - 1).is_err());
    assert!(snapshot.arrival_times(&[1], snapshot.record.event_bytes + 1).is_err());
    assert!(snapshot.arrival_times(&[10, 1], 0).is_err());
    assert!(snapshot.arrival_times(&[1; 51], 0).is_err());
}

#[test]
fn response_and_completion_survive_a_full_event_queue() {
    let f = Fixture::new();
    f.enable(false, false);
    let c = f.begin();
    f.detail(&c);
    let release = f.pause_writer();
    for _ in 0..QUEUE_EVENTS + 1 {
        c.append(Side::Request, b"x");
    }
    assert!(f.recorder.0.queued.load(Ordering::Relaxed) < f.recorder.0.queue_limit.load(Ordering::Relaxed));
    c.response(204, "https://synthetic.example/final", &text_headers());
    c.finish("complete");
    release.send(()).unwrap();
    let record = f.detail(&c);
    assert_eq!(record.response.status, Some(204));
    assert!(!record.response.headers.is_empty());
    assert_eq!(record.state, "complete");
    assert_eq!(record.capture_error.as_deref(), Some("queue-full"));
    assert!(record.request.body.truncated);
}


#[test]
fn default_buffer_records_a_ten_mib_request_burst_while_writer_is_paused() {
    let f = Fixture::new();
    f.enable(false, false);
    let c = f.begin();
    f.detail(&c);
    let release = f.pause_writer();
    let body = vec![b'x'; 10 * MIB as usize];
    c.append(Side::Request, &body);
    c.finish("complete");
    let queued = f.recorder.0.queued.load(Ordering::Relaxed);
    release.send(()).unwrap();
    assert!(queued >= body.len(), "only {queued} bytes admitted for a 10 MiB request");
    let record = f.detail(&c);
    assert_eq!(record.request.body.captured_bytes, body.len() as u64);
    assert!(!record.request.body.truncated);
    assert!(record.capture_error.is_none());
    assert_eq!(f.snapshot(&c).copy_body(Side::Request).unwrap().as_bytes(), body);
}


#[test]
fn buffer_settings_survive_restart() {
    let f = Fixture::new();
    let kept = f.root.path().join("kept");
    let mut settings = Settings {
        enabled: true,
        retain: true,
        hide_auth: false,
        capture_binary: true,
        ..Settings::default()
    };
    for budget in [4, 8, 16, 32, 64, 128] {
        settings.buffer_mib = budget;
        f.recorder.configure(settings.clone()).unwrap();
        let saved = fs::read(kept.join("settings.json")).unwrap();
        assert_eq!(serde_json::from_slice::<Settings>(&saved).unwrap().buffer_mib, budget);
    }
    for invalid in [0, 3, 5, 129, u64::MAX] {
        settings.buffer_mib = invalid;
        assert_eq!(f.recorder.configure(settings.clone()).unwrap_err(), "invalid-network-log-settings");
        assert_eq!(f.recorder.settings().buffer_mib, 128);
    }
    fs::create_dir(kept.join("settings.tmp")).unwrap();
    settings.buffer_mib = 4;
    assert!(f.recorder.configure(settings).is_err());
    assert_eq!(f.recorder.settings().buffer_mib, 128);
    assert_eq!(f.recorder.0.queue_limit.load(Ordering::Relaxed), 128 * MIB as usize);
    f.recorder.shutdown();
    let reopened = Recorder::open(f.root.path().join("recent"), kept).unwrap();
    assert_eq!(reopened.settings().buffer_mib, 128);
    assert!(reopened.settings().capture_binary);
    assert!(!reopened.settings().hide_auth);
    assert_eq!(reopened.0.queue_limit.load(Ordering::Relaxed), 128 * MIB as usize);
    reopened.shutdown();
}

#[test]
fn lowering_the_buffer_preserves_admitted_bytes_and_limits_the_same_active_capture() {
    let f = Fixture::new();
    f.enable(false, false);
    let c = f.begin();
    f.detail(&c);
    let release = f.pause_writer();
    c.append(Side::Request, &vec![b'x'; 10 * MIB as usize]);
    release.send(()).unwrap();
    f.recorder.configure(Settings { enabled: true, buffer_mib: 4, ..Settings::default() }).unwrap();
    let release = f.pause_writer();
    c.append(Side::Request, &vec![b'y'; 6 * MIB as usize]);
    c.finish("complete");
    let queued = f.recorder.0.queued.load(Ordering::Relaxed);
    release.send(()).unwrap();
    assert!(queued <= 4 * MIB as usize);
    let record = f.detail(&c);
    assert_eq!(record.request.body.observed_bytes, 16 * MIB);
    assert!(record.request.body.captured_bytes > 10 * MIB);
    assert!(record.request.body.captured_bytes < 14 * MIB);
    assert_eq!(record.capture_error.as_deref(), Some("queue-full"));
    assert!(record.request.body.truncated);
    let saved = f.snapshot(&c).copy_body(Side::Request).unwrap();
    assert!(saved.as_bytes()[..10 * MIB as usize].iter().all(|byte| *byte == b'x'));
    assert!(saved.as_bytes()[10 * MIB as usize..].iter().all(|byte| *byte == b'y'));
}

#[test]
fn drained_sse_can_exceed_the_shared_buffer_in_total() {
    let f = Fixture::new();
    f.recorder.configure(Settings { enabled: true, buffer_mib: 4, ..Settings::default() }).unwrap();
    let c = f.begin();
    c.response(200, "https://synthetic.example/stream", &text_headers());
    let chunk = vec![b'x'; MIB as usize];
    for _ in 0..12 {
        c.append(Side::Response, &chunk);
        f.detail(&c);
    }
    c.finish("complete");
    let record = f.detail(&c);
    assert_eq!(record.response.body.captured_bytes, 12 * MIB);
    assert!(!record.response.body.truncated);
    assert!(record.capture_error.is_none());
    assert_eq!(f.snapshot(&c).copy_body(Side::Response).unwrap().len(), 12 * MIB as usize);
}

#[test]
fn fixed_body_limit_applies_at_512_mib_to_each_side() {
    let f = Fixture::new();
    f.enable(false, false);
    let c = f.begin();
    let signal = c.0.0.as_ref().unwrap();
    // Seed the admitted prefix to exercise both exact boundaries without allocating 1 GiB.
    for side in [Side::Request, Side::Response] {
        signal.admitted[side.index()].store(512 * MIB - 16, Ordering::Relaxed);
        signal.observed[side.index()].store(512 * MIB - 16, Ordering::Relaxed);
        c.append(side, &[b'z'; 32]);
    }
    c.finish("complete");
    let record = f.detail(&c);
    assert_eq!(record.capture_error.as_deref(), Some("body-limit"));
    for side in [Side::Request, Side::Response] {
        assert_eq!(record.body(side).observed_bytes, 512 * MIB + 16);
        assert_eq!(record.body(side).captured_bytes, 16);
        assert!(record.body(side).truncated);
        assert_eq!(f.snapshot(&c).copy_body(side).unwrap(), "z".repeat(16));
    }
}

#[test]
fn enabled_binary_capture_records_ten_mib_bursts_on_both_sides() {
    use std::io::Read;
    let f = Fixture::new();
    f.recorder.configure(Settings { enabled: true, capture_binary: true, ..Settings::default() }).unwrap();
    let mut headers = http::HeaderMap::new();
    headers.insert("content-type", "application/octet-stream".parse().unwrap());
    let c = f.recorder.begin("synthetic", "POST", "https://synthetic.example", &headers).guard();
    c.response(200, "https://synthetic.example", &headers);
    f.detail(&c);
    let release = f.pause_writer();
    let body = vec![0xff; 10 * MIB as usize];
    for side in [Side::Request, Side::Response] {
        c.append(side, &body);
    }
    c.finish("complete");
    release.send(()).unwrap();
    let record = f.detail(&c);
    assert!(record.capture_error.is_none());
    let mut snapshot = f.snapshot(&c);
    for side in [Side::Request, Side::Response] {
        assert_eq!(record.body(side).captured_bytes, 10 * MIB);
        assert_eq!(record.body(side).observed_bytes, 10 * MIB);
        assert!(!record.body(side).truncated);
        assert!(record.body(side).binary);
        let input = match side { Side::Request => &mut snapshot.request, Side::Response => &mut snapshot.response };
        let mut saved = Vec::new();
        input.read_to_end(&mut saved).unwrap();
        assert_eq!(saved, body);
    }
}

#[test]
fn binary_body_caps_are_cumulative_and_independent_for_request_and_response() {
    let f = Fixture::new();
    f.recorder.configure(Settings { enabled: true, capture_binary: true, buffer_mib: 4, ..Settings::default() }).unwrap();
    let mut headers = http::HeaderMap::new();
    headers.insert("content-type", "image/png".parse().unwrap());
    let c = f.recorder.begin("synthetic", "POST", "https://synthetic.example", &headers).guard();
    c.response(200, "https://synthetic.example", &headers);
    let body = vec![0xff; MIB as usize];
    for _ in 0..5 {
        for side in [Side::Request, Side::Response] {
            c.append(side, &body);
        }
        f.detail(&c);
    }
    c.finish("complete");
    let record = f.detail(&c);
    assert_eq!(record.capture_error.as_deref(), Some("body-limit"));
    for side in [Side::Request, Side::Response] {
        assert_eq!(record.body(side).captured_bytes, 4 * MIB);
        assert_eq!(record.body(side).observed_bytes, 5 * MIB);
        assert!(record.body(side).truncated);
        assert_eq!(f.snapshot(&c).body_range(side, 4 * MIB - 16, 32, None).unwrap().text, "ff".repeat(16));
    }
}

#[test]
fn disabled_websocket_binary_payloads_keep_message_metadata_and_readable_text() {
    let f = Fixture::new();
    f.enable(false, false);
    let c = f.recorder.begin("ws", "WS", "wss://synthetic.example", &http::HeaderMap::new()).guard();
    c.append_message(Side::Response, b"hello", Some("text"));
    c.append_message(Side::Response, &[0xff; 100_000], Some("binary"));
    c.append_message(Side::Response, &[0xff], Some("pong"));
    c.append_message(Side::Response, b"ping", Some("ping"));
    c.append_message(Side::Response, b"world", Some("text"));
    c.finish("complete");
    let record = f.detail(&c);
    assert_eq!(record.response.body.observed_bytes, 100_015);
    assert_eq!(record.response.body.captured_bytes, 14);
    assert!(!record.response.body.binary);
    assert!(record.response.body.truncated);
    assert_eq!(record.capture_error.as_deref(), Some("binary-skipped"));
    assert_eq!(f.snapshot(&c).copy_body(Side::Response).unwrap(), "hellopingworld");
    let mut json = Vec::new();
    f.snapshot(&c).export_json(&mut json).unwrap();
    let json: serde_json::Value = serde_json::from_slice(&json).unwrap();
    assert_eq!(json["response"]["body"]["encoding"], "utf8");
    assert_eq!(json["response"]["body"]["data"], "hellopingworld");
    let events = json["stream"]["observations"].as_array().unwrap();
    assert_eq!(events.len(), 5);
    assert_eq!(events[1]["kind"], "binary");
    assert_eq!(events[1]["length"], 0);
    assert_eq!(events[2]["kind"], "pong");
    assert_eq!(events[2]["length"], 0);
    assert_eq!(events[3]["offset"], 5);
    assert_eq!(events[4]["offset"], 9);
}

#[test]
fn websocket_binary_cap_does_not_stop_later_text_and_readable_controls() {
    let f = Fixture::new();
    f.recorder.configure(Settings { enabled: true, capture_binary: true, buffer_mib: 4, ..Settings::default() }).unwrap();
    let c = f.recorder.begin("ws", "WS", "wss://synthetic.example", &http::HeaderMap::new()).guard();
    let body = vec![0xff; MIB as usize];
    for _ in 0..5 {
        for side in [Side::Request, Side::Response] {
            c.append_message(side, &body, Some("binary"));
        }
        f.detail(&c);
    }
    for side in [Side::Request, Side::Response] {
        c.append_message(side, &[0xff], Some("pong"));
        c.append_message(side, b"ping", Some("ping"));
        c.append_message(side, b"tail", Some("text"));
    }
    c.finish("complete");
    let record = f.detail(&c);
    assert_eq!(record.capture_error.as_deref(), Some("body-limit"));
    for side in [Side::Request, Side::Response] {
        assert_eq!(record.body(side).captured_bytes, 4 * MIB + 8);
        assert_eq!(record.body(side).observed_bytes, 5 * MIB + 9);
        assert!(record.body(side).binary);
        assert_eq!(f.snapshot(&c).body_range(side, 4 * MIB, 8, Some(false)).unwrap().text, "pingtail");
    }
}

#[test]
fn binary_option_and_cap_apply_to_new_requests_and_preserve_saved_bodies() {
    let f = Fixture::new();
    f.enable(true, false);
    let mut headers = http::HeaderMap::new();
    headers.insert("content-type", "application/octet-stream".parse().unwrap());
    let off = f.recorder.begin("synthetic", "POST", "https://synthetic.example", &headers).guard();
    let enabled = Settings { enabled: true, retain: true, capture_binary: true, buffer_mib: 4, ..Settings::default() };
    f.recorder.configure(enabled).unwrap();
    let on = f.recorder.begin("synthetic", "POST", "https://synthetic.example", &headers).guard();
    f.recorder.configure(Settings { enabled: true, retain: true, buffer_mib: 8, ..Settings::default() }).unwrap();
    let off_again = f.recorder.begin("synthetic", "POST", "https://synthetic.example", &headers).guard();
    let body = vec![0xff; MIB as usize];
    for _ in 0..5 {
        for capture in [&off, &on, &off_again] {
            capture.append(Side::Request, &body);
        }
        f.detail(&on);
    }
    for capture in [&off, &on, &off_again] { capture.finish("complete"); }
    assert_eq!(f.detail(&off).request.body.captured_bytes, 0);
    assert_eq!(f.detail(&off_again).request.body.captured_bytes, 0);
    let record = f.detail(&on);
    assert_eq!(record.request.body.captured_bytes, 4 * MIB);
    assert_eq!(record.request.body.observed_bytes, 5 * MIB);
    assert_eq!(record.capture_error.as_deref(), Some("body-limit"));
    f.recorder.shutdown();
    let reopened = Recorder::open(f.root.path().join("recent"), f.root.path().join("kept")).unwrap();
    assert!(!reopened.settings().capture_binary);
    let mut snapshot = reopened.snapshot(record.id.clone()).unwrap();
    assert_eq!(snapshot.record.request.body.captured_bytes, 4 * MIB);
    assert_eq!(snapshot.body_range(Side::Request, 0, 16, None).unwrap().text, "ff".repeat(16));
    let mut output = Vec::new();
    reopened.snapshot(record.id).unwrap().export_json(&mut output).unwrap();
    let json: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["request"]["body"]["encoding"], "hex");
    assert_eq!(json["request"]["body"]["data"], "ff".repeat(4 * MIB as usize));
    drop(snapshot);
    reopened.shutdown();
}

#[test]
fn enabled_binary_capture_stays_best_effort_when_the_shared_queue_is_full() {
    let f = Fixture::new();
    f.recorder.configure(Settings { enabled: true, capture_binary: true, buffer_mib: 4, ..Settings::default() }).unwrap();
    let mut headers = http::HeaderMap::new();
    headers.insert("content-type", "application/octet-stream".parse().unwrap());
    let c = f.recorder.begin("synthetic", "POST", "https://synthetic.example", &headers).guard();
    f.detail(&c);
    let release = f.pause_writer();
    c.append(Side::Request, &vec![0xff; 8 * MIB as usize]);
    c.response(200, "https://synthetic.example", &headers);
    let queued = f.recorder.0.queued.load(Ordering::Relaxed);
    release.send(()).unwrap();
    f.detail(&c);
    c.append(Side::Response, &[0xff; 100_000]);
    c.finish("complete");
    let record = f.detail(&c);
    assert!(queued <= 4 * MIB as usize);
    assert!(record.request.body.captured_bytes > 0 && record.request.body.captured_bytes < 4 * MIB);
    assert_eq!(record.request.body.observed_bytes, 8 * MIB);
    assert_eq!(record.response.body.captured_bytes, 100_000);
    assert_eq!(record.response.status, Some(200));
    assert_eq!(record.capture_error.as_deref(), Some("queue-full"));
}

#[test]
fn a_clean_completion_clears_the_warning_unless_another_capture_is_still_failing() {
    let f = Fixture::new();
    f.recorder.configure(Settings { enabled: true, buffer_mib: 4, ..Settings::default() }).unwrap();
    let overflowed = f.begin();
    f.detail(&overflowed);
    let release = f.pause_writer();
    overflowed.append(Side::Request, &vec![b'x'; 6 * MIB as usize]);
    release.send(()).unwrap();
    assert_eq!(f.recorder.list(Query::default()).unwrap().warning.as_deref(), Some("queue-full"));
    let clean = f.begin();
    clean.append(Side::Request, b"ok");
    clean.finish("complete");
    f.detail(&clean);
    assert_eq!(f.recorder.list(Query::default()).unwrap().warning.as_deref(), Some("queue-full"));
    overflowed.finish("complete");
    f.detail(&overflowed);
    let again = f.begin();
    again.finish("complete");
    f.detail(&again);
    assert_eq!(f.recorder.list(Query::default()).unwrap().warning, None);
}

#[test]
fn oversized_metadata_is_reported_without_stopping_capture() {
    let f = Fixture::new();
    f.enable(false, false);
    let url = format!("https://synthetic.example/{}", "x".repeat(9000));
    let c = f.recorder.begin("synthetic", "GET", &url, &http::HeaderMap::new()).guard();
    assert!(c.is_active());
    c.response(200, "https://synthetic.example", &text_headers());
    c.append(Side::Response, b"data: kept\n\n");
    c.finish("complete");
    let record = f.detail(&c);
    assert_eq!(record.request.url.len(), 8192);
    assert_eq!(record.response.status, Some(200));
    assert_eq!(record.capture_error.as_deref(), Some("metadata-limit"));
    assert_eq!(f.snapshot(&c).copy_body(Side::Response).unwrap(), "data: kept\n\n");
}
