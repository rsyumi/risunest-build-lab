use super::*;
use crate::server_sync::media::MediaProvider;
use futures::StreamExt;
use risunest_sync_connect::media::MediaObject;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Observes the real server's admission from outside it. A held `/head` body
/// does not reach EOF, so the server keeps that request's device slot, until
/// the gate opens.
struct Admission {
    gate: tokio::sync::watch::Sender<bool>,
    held: tokio::sync::watch::Sender<usize>,
    release_on_refusal: AtomicBool,
    refusals: AtomicUsize,
    sessions: AtomicUsize,
    accesses: AtomicUsize,
    inject: std::sync::Mutex<Option<(u16, &'static str)>>,
}

impl Admission {
    /// Counts only what follows binding and eviction.
    fn reset(&self) {
        for counter in [&self.refusals, &self.sessions, &self.accesses] {
            counter.store(0, Ordering::SeqCst);
        }
    }
}

fn admission_fixture() -> (Fixture, Arc<Admission>) {
    let state = Arc::new(Admission {
        gate: tokio::sync::watch::channel(false).0,
        held: tokio::sync::watch::channel(0).0,
        release_on_refusal: AtomicBool::new(false),
        refusals: AtomicUsize::new(0),
        sessions: AtomicUsize::new(0),
        accesses: AtomicUsize::new(0),
        inject: std::sync::Mutex::new(None),
    });
    let root = tempfile::tempdir().unwrap();
    let server = Arc::new(Store::init(root.path()).unwrap());
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let listener = runtime
        .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
        .unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let observed = state.clone();
    let entered = runtime.enter();
    let router = http::router(server.clone()).layer(axum::middleware::from_fn(
        move |request: axum::extract::Request, next: axum::middleware::Next| {
            let state = observed.clone();
            async move {
                let path = request.uri().path().to_owned();
                let hold = path == "/head" && request.headers().contains_key("x-test-hold");
                if path == "/session" {
                    state.sessions.fetch_add(1, Ordering::SeqCst);
                }
                if path == "/media/access" {
                    state.accesses.fetch_add(1, Ordering::SeqCst);
                    let injected = *state.inject.lock().unwrap();
                    if let Some((status, body)) = injected {
                        return axum::response::Response::builder()
                            .status(status)
                            .body(axum::body::Body::from(body))
                            .unwrap();
                    }
                }
                let response = next.run(request).await;
                if response.status() == 429 {
                    state.refusals.fetch_add(1, Ordering::SeqCst);
                    if state.release_on_refusal.load(Ordering::SeqCst) {
                        // The refused client sees its 429 only after both held
                        // bodies ended, so the slots are free when it retries.
                        let mut held = state.held.subscribe();
                        state.gate.send_replace(true);
                        held.wait_for(|count| *count == 0).await.unwrap();
                    }
                }
                if !hold || response.status() != 200 {
                    return response;
                }
                state.held.send_modify(|count| *count += 1);
                let mut gate = state.gate.subscribe();
                let ended = state.clone();
                let (parts, body) = response.into_parts();
                let opened = futures::stream::once(async move {
                    let _ = gate.wait_for(|open| *open).await;
                })
                .filter_map(|_| async { None });
                let finished = futures::stream::once(async move {
                    ended.held.send_modify(|count| *count -= 1);
                })
                .filter_map(|_| async { None });
                let stream = opened.chain(body.into_data_stream()).chain(finished);
                axum::response::Response::from_parts(parts, axum::body::Body::from_stream(stream))
            }
        },
    ));
    drop(entered);
    let task = runtime.spawn(async move { axum::serve(listener, router).await.unwrap() });
    (
        Fixture {
            _server_root: root,
            server,
            runtime,
            task,
            endpoint,
        },
        state,
    )
}

struct Remote {
    _root: tempfile::TempDir,
    _store: PersistentStore,
    config: ServerConfig,
    provider: Arc<MediaProvider>,
    hash: String,
    size: u64,
    bytes: &'static [u8],
}
impl Remote {
    fn new(fixture: &Fixture) -> Self {
        let (root, mut store) = prepared();
        let device = fixture.server.add_device().unwrap();
        let config = ServerConfig {
            directory: None,
            endpoint: fixture.endpoint.clone(),
            library_id: device.library_id,
            device_id: device.device_id,
            token: device.token,
        };
        store.server_bind(&config).unwrap();
        let bytes: &'static [u8] = b"synthetic admission";
        let alias = put(&mut store, "assets/admission.png", bytes);
        store
            .asset_residency_set_policy(AssetPolicy::Remote, || Ok(()))
            .unwrap();
        store.asset_residency_evict(|| Ok(())).unwrap();
        let provider = Arc::new(
            MediaProvider::new(
                store.repository_root().to_owned(),
                "http://127.0.0.1:8123".into(),
            )
            .unwrap(),
        );
        Self {
            _root: root,
            _store: store,
            config,
            provider,
            hash: alias.object_hash.unwrap(),
            size: alias.size as u64,
            bytes,
        }
    }
    fn object(&self, mime: &str) -> MediaObject {
        MediaObject {
            hash: self.hash.clone(),
            size: self.size.into(),
            mime: mime.into(),
        }
    }
    /// Occupies one of the device's request slots with a response whose body
    /// has not reached EOF.
    fn hold(&self, state: &Admission) -> reqwest::blocking::Response {
        let before = *state.held.borrow();
        let response = reqwest::blocking::Client::new()
            .get(format!("{}/head", self.config.endpoint))
            .bearer_auth(&self.config.token)
            .header("x-risu-library", &self.config.library_id)
            .header("x-test-hold", "1")
            .send()
            .unwrap();
        assert_eq!(response.status().as_u16(), 200);
        assert_eq!(*state.held.borrow(), before + 1);
        response
    }
}

fn finish(held: Vec<reqwest::blocking::Response>) {
    for response in held {
        assert!(!response.bytes().unwrap().is_empty());
    }
}

#[test]
fn a_media_grant_is_issued_once_held_response_bodies_release_admission() {
    let (fixture, state) = admission_fixture();
    let remote = Remote::new(&fixture);
    state.reset();
    state.release_on_refusal.store(true, Ordering::SeqCst);
    let held = vec![remote.hold(&state), remote.hold(&state)];
    let url = match remote.provider.url(&remote.object("image/png"), false) {
        Ok(url) => url,
        Err(error) => panic!("code={} status={}", error.code, error.status),
    };
    assert!(url.starts_with(&fixture.endpoint));
    assert_eq!(state.refusals.load(Ordering::SeqCst), 1);
    assert_eq!(state.sessions.load(Ordering::SeqCst), 2);
    assert_eq!(state.accesses.load(Ordering::SeqCst), 1);
    assert_eq!(*state.held.borrow(), 0);
    finish(held);
    let body = reqwest::blocking::get(&url).unwrap();
    assert_eq!(body.status().as_u16(), 200);
    assert_eq!(body.bytes().unwrap().as_ref(), remote.bytes);
}

#[test]
fn eight_mime_variants_all_receive_grants_after_contended_admission() {
    let (fixture, state) = admission_fixture();
    let remote = Remote::new(&fixture);
    state.reset();
    state.release_on_refusal.store(true, Ordering::SeqCst);
    let held = vec![remote.hold(&state), remote.hold(&state)];
    let barrier = Arc::new(std::sync::Barrier::new(8));
    let tasks = (0..8)
        .map(|i| {
            let provider = remote.provider.clone();
            let barrier = barrier.clone();
            let object = remote.object(&format!("image/png; variant={i}"));
            std::thread::spawn(move || {
                barrier.wait();
                provider.url(&object, false)
            })
        })
        .collect::<Vec<_>>();
    let mut urls = std::collections::BTreeSet::new();
    for task in tasks {
        match task.join().unwrap() {
            Ok(url) => assert!(urls.insert(url)),
            Err(error) => panic!("code={} status={}", error.code, error.status),
        }
    }
    assert_eq!(urls.len(), 8);
    assert!(state.refusals.load(Ordering::SeqCst) >= 1);
    assert_eq!(state.accesses.load(Ordering::SeqCst), 8);
    finish(held);
}

/// Closing the last residency connection tears down its write-ahead log, and
/// concurrent lookups racing that teardown fail. The provider keeps it open.
#[test]
fn media_lookups_keep_the_residency_log_open_between_requests() {
    let (fixture, _state) = admission_fixture();
    let remote = Remote::new(&fixture);
    let log = format!(
        "{}-wal",
        crate::server_sync::residency::Residency::path(remote._store.repository_root()).display()
    );
    assert!(!std::path::Path::new(&log).exists());
    remote.provider.url(&remote.object("image/png"), false).unwrap();
    remote.provider.url(&remote.object("image/webp"), false).unwrap();
    assert!(std::path::Path::new(&log).is_file());
}

#[test]
fn exhausted_admission_is_retryable_and_leaves_no_grant_behind() {
    let (fixture, state) = admission_fixture();
    let remote = Remote::new(&fixture);
    state.reset();
    let held = vec![remote.hold(&state), remote.hold(&state)];
    let object = remote.object("image/png");
    let error = remote.provider.url(&object, false).unwrap_err();
    assert_eq!(
        (error.code.as_str(), error.status, error.retryable),
        ("admission-unavailable", 503, true)
    );
    assert_eq!(state.refusals.load(Ordering::SeqCst), 3);
    assert_eq!(state.sessions.load(Ordering::SeqCst), 3);
    assert_eq!(state.accesses.load(Ordering::SeqCst), 0);
    assert_eq!(*state.held.borrow(), 2);

    state.gate.send_replace(true);
    finish(held);
    remote.provider.url(&object, false).unwrap();
    assert_eq!(state.sessions.load(Ordering::SeqCst), 4);
    assert_eq!(state.accesses.load(Ordering::SeqCst), 1);
}

#[test]
fn a_valid_grant_is_reused_without_a_request_while_admission_is_held() {
    let (fixture, state) = admission_fixture();
    let remote = Remote::new(&fixture);
    state.reset();
    let object = remote.object("image/png");
    let url = remote.provider.url(&object, false).unwrap();
    let held = vec![remote.hold(&state), remote.hold(&state)];
    assert_eq!(remote.provider.url(&object, false).unwrap(), url);
    assert_eq!(state.sessions.load(Ordering::SeqCst), 1);
    assert_eq!(state.accesses.load(Ordering::SeqCst), 1);
    assert_eq!(state.refusals.load(Ordering::SeqCst), 0);
    state.gate.send_replace(true);
    finish(held);
}

#[test]
fn an_unrecognized_or_permanent_media_access_refusal_is_not_replayed() {
    let (fixture, state) = admission_fixture();
    let remote = Remote::new(&fixture);
    state.reset();
    let object = remote.object("image/png");
    for (index, (status, body, code)) in [
        (429, r#"{"error":"rate-limited"}"#, "rate-limited"),
        (429, "Too Many Requests", "server-response-error"),
        (403, r#"{"error":"forbidden"}"#, "forbidden"),
    ]
    .into_iter()
    .enumerate()
    {
        *state.inject.lock().unwrap() = Some((status, body));
        let observed = match remote.provider.url(&object, false) {
            Ok(_) => None,
            Err(error) => Some((error.code, error.status)),
        };
        assert!(
            observed.as_ref().is_some_and(|(actual, actual_status)| {
                actual == code && *actual_status == status
            }),
            "iteration={index} observed={observed:?} sessions={} refusals={} accesses={}",
            state.sessions.load(Ordering::SeqCst),
            state.refusals.load(Ordering::SeqCst),
            state.accesses.load(Ordering::SeqCst)
        );
        assert_eq!(state.accesses.load(Ordering::SeqCst), index + 1);
    }
    *state.inject.lock().unwrap() = None;
    remote.provider.url(&object, false).unwrap();
    assert_eq!(state.accesses.load(Ordering::SeqCst), 4);
}
