use super::{
    cache::Cache,
    client::{ServerClient, ServerConfig, TestIoCounters},
    lww_tests::{drain_publications, local, receive_available, save, LocalServerFixture},
    transfer::Transfer,
};
use crate::persistent_store::{ConversationMutation, WorkingSetCommit};
use risunest_sync_wire::body as body_codec;
use serde_json::json;
use std::{
    collections::BTreeSet,
    sync::{atomic::Ordering, Arc, Mutex},
};

const MIB: usize = 1024 * 1024;

/// Prose-like text: words drawn at random from a small vocabulary, so it
/// compresses like writing rather than like one repeated line.
fn text(len: usize, seed: usize) -> String {
    const WORDS: [&str; 64] = [
        "the", "a", "river", "night", "she", "he", "they", "said", "quietly", "into",
        "lantern", "road", "before", "after", "smiled", "turned", "door", "window", "rain",
        "letter", "old", "new", "remember", "forgot", "was", "were", "is", "not", "never",
        "always", "under", "over", "between", "voice", "hand", "eyes", "morning", "evening",
        "garden", "stone", "bridge", "across", "toward", "away", "slowly", "suddenly",
        "laughed", "whispered", "answered", "asked", "city", "forest", "sea", "ship",
        "promise", "secret", "warm", "cold", "bright", "dark", "small", "long", "of", "and",
    ];
    let mut state = 0x2545_f491_4f6c_dd1du64 ^ (seed as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    let mut text = String::with_capacity(len + 16);
    while text.len() < len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        text.push_str(WORDS[(state % 64) as usize]);
        text.push(if state % 11 == 0 { '.' } else { ' ' });
    }
    text.truncate(len);
    text
}

/// Random URL-safe characters, as base64 media inside library JSON looks.
pub(crate) fn random_text(len: usize, seed: usize) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut state = 0x2545_f491_4f6c_dd1du64 ^ (seed as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            ALPHABET[(state % 64) as usize] as char
        })
        .collect()
}

/// Incompressible bytes, for tests that compare wire bytes with raw sizes.
pub(crate) fn noise(len: usize) -> Vec<u8> {
    let mut state = 0x9e37_79b9_7f4a_7c15u64;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect()
}

#[derive(Debug, Default)]
struct Traffic {
    request_wire: u64,
    request_raw: u64,
    response_wire: u64,
    response_raw: u64,
    expanded: u64,
    journal_raw: u64,
    part_requests: u64,
    part_raw: u64,
    transfer_raw: u64,
}

/// A route as the server saw it, then whether the request and the reply
/// carried the body marker.
type Exchange = (&'static str, bool, bool);

struct Run {
    traffic: Traffic,
    exchanges: BTreeSet<Exchange>,
}

pub(crate) fn route(path: &str) -> &'static str {
    match path {
        "/push" => "push",
        "/changes" => "changes",
        "/uploads/frames" => "frames",
        "/objects/transfer" => "transfer",
        _ if path.starts_with("/objects/") && path.ends_with("/part") => "part",
        _ if path.starts_with("/uploads/") && path.contains("/chunks/") => "chunk",
        _ if path.starts_with("/uploads/") && path.ends_with("/delta") => "upload-delta",
        _ if path.starts_with("/object-deltas/") => "object-delta",
        _ => "other",
    }
}

fn traffic(counters: &[&TestIoCounters]) -> Traffic {
    let mut total = Traffic::default();
    for counter in counters {
        let load = |value: &std::sync::atomic::AtomicU64| value.load(Ordering::Relaxed);
        total.request_wire += load(&counter.request_body_bytes);
        total.request_raw += load(&counter.request_raw_bytes);
        total.response_wire += load(&counter.response_body_bytes);
        total.response_raw += load(&counter.response_raw_bytes);
        total.expanded += load(&counter.expanded_bodies);
        let replies = counter.traffic.lock().unwrap();
        total.journal_raw += replies.journal_response_raw_bytes;
        total.part_requests += replies.object_part_requests;
        total.part_raw += replies.object_part_response_raw_bytes;
        total.transfer_raw += replies.object_transfer_response_raw_bytes;
    }
    total
}

type Exchanges = Arc<Mutex<BTreeSet<Exchange>>>;

fn recording_server() -> (LocalServerFixture, Exchanges) {
    let exchanges = Exchanges::default();
    let seen = exchanges.clone();
    let server = LocalServerFixture::with_router(move |router| {
        router.layer(axum::middleware::from_fn(
            move |request: axum::extract::Request, next: axum::middleware::Next| {
                let seen = seen.clone();
                async move {
                    let route = route(request.uri().path());
                    let sent = request.headers().contains_key(body_codec::ENCODING_HEADER);
                    let response = next.run(request).await;
                    let received = response.headers().contains_key(body_codec::ENCODING_HEADER);
                    seen.lock().unwrap().insert((route, sent, received));
                    response
                }
            },
        ))
    });
    (server, exchanges)
}

fn device_client(server: &LocalServerFixture) -> (ServerClient, Arc<TestIoCounters>) {
    let credential = server.server.add_device().unwrap();
    let mut client = ServerClient::new(ServerConfig {
        directory: None,
        endpoint: server.endpoint.clone(),
        library_id: credential.library_id,
        device_id: credential.device_id,
        token: credential.token,
    })
    .unwrap();
    let counters = Arc::new(TestIoCounters::default());
    client.test_io = Some(counters.clone());
    (client, counters)
}

/// Syncs inline units, message pages, a chunked unit and an edit of it from
/// one store to another.
fn sync_library(codec: bool, content: fn(usize, usize) -> String) -> Run {
    let (server, exchanges) = recording_server();
    let (_a, mut a) = local();
    let (_b, mut b) = local();
    let ca = server.client(&a);
    let cb = server.client(&b);
    for client in [&ca, &cb] {
        let counters = client.client.test_io.as_ref().unwrap();
        counters.codec_disabled.store(!codec, Ordering::Relaxed);
    }
    for (seed, field) in ["additionalPrompt", "NAIImgUrl", "autofillRequestUrl", "language", "ImagenModel"]
        .into_iter()
        .enumerate()
    {
        save(&mut a, &["root", field], json!(content(64 * 1024, seed)));
    }
    save(&mut a, &["exists", "character", "char"], json!({"type":"character"}));
    save(&mut a, &["exists", "conversation", "char", "chat"], json!(true));
    a.commit(&WorkingSetCommit {
        expected_revision: a.revision().unwrap(),
        conversations: Some(vec![ConversationMutation::ReplaceRange {
            character_id: "char".into(),
            conversation_id: "chat".into(),
            start: 0,
            delete_count: 0,
            messages: (0..600)
                .map(|index| json!({"role":"user","data":content(1024, 100 + index)}))
                .collect(),
            conversation: None,
            configured_index: None,
        }]),
        ..Default::default()
    })
    .unwrap();
    let large = content(6 * MIB, 1000);
    save(&mut a, &["root", "customCSS"], json!(large));
    drain_publications(&ca, &mut a, &[]).unwrap();
    receive_available(&cb, &mut b, &[]).unwrap();
    let mut edited = large;
    edited.insert_str(3 * MIB, &content(64 * 1024, 2000));
    save(&mut a, &["root", "customCSS"], json!(edited));
    drain_publications(&ca, &mut a, &[]).unwrap();
    receive_available(&cb, &mut b, &[]).unwrap();
    // Compare digests so a mismatch does not print megabytes of text.
    let root = |store: &crate::persistent_store::PersistentStore| {
        store.read_root(None).unwrap().value.as_object().unwrap().iter()
            .map(|(key, value)| (key.clone(), risunest_sync_wire::hash(value.to_string().as_bytes())))
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    assert_eq!(root(&a), root(&b));
    let messages = |store: &crate::persistent_store::PersistentStore| {
        store.read_conversation("char", "chat", None).unwrap().unwrap().value["message"].clone()
    };
    assert_eq!(messages(&a).as_array().unwrap().len(), 600);
    assert_eq!(messages(&a), messages(&b));
    let traffic = traffic(&[
        ca.client.test_io.as_ref().unwrap(),
        cb.client.test_io.as_ref().unwrap(),
    ]);
    let exchanges = std::mem::take(&mut *exchanges.lock().unwrap());
    Run { traffic, exchanges }
}

fn ratio(name: &str, traffic: &Traffic) -> f64 {
    let wire = traffic.request_wire + traffic.response_wire;
    let raw = traffic.request_raw + traffic.response_raw;
    let ratio = wire as f64 / raw as f64;
    eprintln!(
        "{name}: requests {} of {} bytes, replies {} of {} bytes, ratio {ratio:.3}",
        traffic.request_wire, traffic.request_raw, traffic.response_wire, traffic.response_raw,
    );
    ratio
}

#[test]
fn a_text_heavy_library_moves_at_most_half_its_raw_bytes() {
    let raw = sync_library(false, text);
    let encoded = sync_library(true, text);
    let (raw, routes) = (raw.traffic, raw.exchanges);
    assert!(raw.journal_raw > 0 && raw.transfer_raw > 0 && raw.part_requests > 0, "{raw:?}");
    for route in ["push", "changes", "frames", "transfer", "part", "chunk"] {
        assert!(routes.iter().any(|exchange| exchange.0 == route), "{route}: {routes:?}");
    }
    assert!(routes.iter().all(|&(_, sent, received)| !sent && !received), "{routes:?}");
    for route in ["push", "frames", "chunk"] {
        assert!(encoded.exchanges.iter().any(|exchange| exchange.0 == route && exchange.1), "{route}");
    }
    for route in ["changes", "transfer", "part"] {
        assert!(encoded.exchanges.iter().any(|exchange| exchange.0 == route && exchange.2), "{route}");
    }
    let encoded = encoded.traffic;
    // Replies that carry library content decode to the same bytes either way.
    assert_eq!(
        (encoded.journal_raw, encoded.part_raw, encoded.transfer_raw),
        (raw.journal_raw, raw.part_raw, raw.transfer_raw)
    );
    assert_eq!((raw.request_wire, raw.response_wire), (raw.request_raw, raw.response_raw));
    assert_eq!((raw.expanded, encoded.expanded), (0, 0));
    assert!(encoded.request_wire * 2 <= encoded.request_raw, "{encoded:?}");
    assert!(encoded.response_wire * 2 <= encoded.response_raw, "{encoded:?}");
    assert!(ratio("text library", &encoded) <= 0.5);
}

#[test]
fn a_library_of_random_text_never_grows_a_body() {
    let random = sync_library(true, random_text).traffic;
    assert_eq!(random.expanded, 0, "{random:?}");
    assert!(ratio("random text library", &random) < 1.0);
}

#[test]
fn incompressible_objects_travel_raw_through_chunks_and_parts() {
    let server = LocalServerFixture::new();
    let (client, counters) = device_client(&server);
    let payload = noise(6 * MIB);
    let (source, target) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let source = Cache::open(source.path()).unwrap();
    let target = Cache::open(target.path()).unwrap();
    let hash = source.put(&payload).unwrap();
    Transfer::new(&client, &source)
        .unwrap()
        .upload(std::slice::from_ref(&hash), &[])
        .unwrap();
    Transfer::new(&client, &target)
        .unwrap()
        .download(std::slice::from_ref(&hash), &[])
        .unwrap();
    assert_eq!(target.read(&hash, payload.len()).unwrap(), payload);
    let measured = traffic(&[&counters]);
    assert!(measured.part_requests >= 6, "{measured:?}");
    assert!(measured.request_raw >= payload.len() as u64);
    assert_eq!(measured.part_raw, payload.len() as u64);
    assert_eq!(
        (measured.request_wire, measured.response_wire, measured.expanded),
        (measured.request_raw, measured.response_raw, 0),
        "{measured:?}"
    );
}

#[test]
fn deltas_against_known_bases_travel_encoded_both_ways() {
    let (server, exchanges) = recording_server();
    let (client, counters) = device_client(&server);
    let (source, target) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let source = Cache::open(source.path()).unwrap();
    let target = Cache::open(target.path()).unwrap();
    let raw = |counter: &std::sync::atomic::AtomicU64| counter.load(Ordering::Relaxed);
    // Below the delta target limit an edit travels as an RNSD frame, above it
    // as an RNSL recipe.
    for (seed, len) in [(1, 6 * MIB), (2, 17 * MIB)] {
        let base = text(len, seed);
        let mut edited = base.clone();
        edited.insert_str(len / 2, &text(64 * 1024, 10 + seed));
        let base = source.put(base.as_bytes()).unwrap();
        let edited_hash = source.put(edited.as_bytes()).unwrap();
        let upload = Transfer::new(&client, &source).unwrap();
        upload.upload(std::slice::from_ref(&base), &[]).unwrap();
        let sent = raw(&counters.request_raw_bytes);
        upload
            .upload(std::slice::from_ref(&edited_hash), std::slice::from_ref(&base))
            .unwrap();
        assert!(raw(&counters.request_raw_bytes) - sent < MIB as u64, "{len}");
        let download = Transfer::new(&client, &target).unwrap();
        download.download(std::slice::from_ref(&base), &[]).unwrap();
        let received = raw(&counters.response_raw_bytes);
        download
            .download(std::slice::from_ref(&edited_hash), std::slice::from_ref(&base))
            .unwrap();
        assert!(raw(&counters.response_raw_bytes) - received < MIB as u64, "{len}");
        assert!(target.read(&edited_hash, len + MIB).unwrap() == edited.as_bytes());
    }
    let exchanges = exchanges.lock().unwrap();
    for (route, request) in [("frames", true), ("transfer", false), ("upload-delta", true), ("object-delta", false)] {
        assert!(
            exchanges.iter().any(|&(seen, sent, received)| seen == route && if request { sent } else { received }),
            "{route}: {exchanges:?}"
        );
    }
    assert_eq!(counters.expanded_bodies.load(Ordering::Relaxed), 0);
}

#[test]
fn encoded_replies_are_checked_before_callers_see_them() {
    use axum::{
        body::{Body, Bytes},
        http::HeaderValue,
        response::Response,
        routing::get,
        Router,
    };
    use std::sync::atomic::AtomicUsize;
    let raw = text(4096, 7).into_bytes();
    let encoded = body_codec::encode(&raw).unwrap().unwrap();
    let gzip = {
        use std::io::Write;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(&raw).unwrap();
        encoder.finish().unwrap()
    };
    let marked = |bytes: Vec<u8>, markers: usize| {
        let mut response = Response::new(Body::from(bytes));
        for _ in 0..markers {
            response.headers_mut().append(
                body_codec::ENCODING_HEADER,
                HeaderValue::from_static(body_codec::ZSTD),
            );
        }
        response
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let router = Router::new()
        .route("/encoded", get({
            let encoded = encoded.clone();
            move || {
                let encoded = encoded.clone();
                async move { marked(encoded, 1) }
            }
        }))
        .route("/repeated", get({
            let encoded = encoded.clone();
            move || {
                let encoded = encoded.clone();
                async move { marked(encoded, 2) }
            }
        }))
        .route("/malformed", get({
            let calls = calls.clone();
            move || {
                calls.fetch_add(1, Ordering::SeqCst);
                async move { marked(b"not a zstd frame".to_vec(), 1) }
            }
        }))
        .route("/long", get(move || async move { marked(noise(2048), 1) }))
        .route("/gzip", get(move || async move {
            let mut response = Response::new(Body::from(gzip));
            response
                .headers_mut()
                .insert("content-encoding", HeaderValue::from_static("gzip"));
            response
        }))
        .route("/cut", get({
            let calls = calls.clone();
            let encoded = encoded.clone();
            move || {
                let first = calls.fetch_add(1, Ordering::SeqCst) == 0;
                let encoded = encoded.clone();
                async move {
                    if !first {
                        return marked(encoded, 1);
                    }
                    let (head, _) = encoded.split_at(encoded.len() / 2);
                    let chunks: Vec<Result<Bytes, std::io::Error>> = vec![
                        Ok(Bytes::copy_from_slice(head)),
                        Err(std::io::Error::other("synthetic cut")),
                    ];
                    let mut response = marked(Vec::new(), 1);
                    *response.body_mut() = Body::from_stream(futures::stream::iter(chunks));
                    response
                        .headers_mut()
                        .insert("content-length", encoded.len().into());
                    response
                }
            }
        }));
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let listener = runtime
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = runtime.spawn(async move { axum::serve(listener, router).await.unwrap() });
    let client = ServerClient::new(ServerConfig {
        directory: None,
        endpoint,
        library_id: "library".into(),
        device_id: "device".into(),
        token: "a".repeat(64),
    })
    .unwrap();
    let get = |path: &str, limit: usize| {
        client.request(reqwest::Method::GET, path, &[], None, &[], limit)
    };

    let reply = get("encoded", raw.len()).unwrap();
    assert_eq!((reply.status, reply.body), (200, raw.clone()));
    for (path, limit, code) in [
        ("repeated", raw.len(), "invalid-response-encoding"),
        ("malformed", raw.len(), "invalid-response-encoding"),
        // Decodes past the caller's limit.
        ("encoded", raw.len() - 1, "response-too-large"),
        // Longer than the limit before decoding.
        ("long", 1024, "response-too-large"),
    ] {
        let error = get(path, limit).err().unwrap();
        assert_eq!(
            (error.code.as_str(), error.status, error.retryable),
            (code, 502, false),
            "{path}"
        );
    }
    assert_eq!(calls.swap(0, Ordering::SeqCst), 1, "a malformed frame is not retried");
    // An HTTP content coding is refused, not decoded on the way in.
    let error = get("gzip", raw.len()).err().unwrap();
    assert_eq!(
        (error.code.as_str(), error.status),
        ("unexpected-content-encoding", 502)
    );
    // A body cut mid-transfer is retried and then decoded.
    let reply = get("cut", raw.len()).unwrap();
    assert_eq!((reply.status, reply.body), (200, raw));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    server.abort();
}
