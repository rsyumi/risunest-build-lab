use super::*;
use tokio::sync::{mpsc, watch};

struct Fixture {
    _root: tempfile::TempDir,
    app: App,
    shutdown: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
    client: reqwest::Client,
    endpoint: String,
    library: String,
    token: String,
    entered: mpsc::UnboundedReceiver<()>,
    release: Arc<Semaphore>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Fixture {
    async fn new(buffers: usize) -> Self {
        let root = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::init(root.path()).unwrap());
        let device = store.add_device().unwrap();
        let (shutdown, receiver) = watch::channel(false);
        let app = App {
            store,
            buffers: Arc::new(Semaphore::new(buffers)),
            materializers: Arc::new(Semaphore::new(1)),
            media_slots: Arc::new(Semaphore::new(16)),
            workload: Workload::new(),
            shutdown: receiver,
            _lifetime: Arc::new(()),
        };
        let (sender, entered) = mpsc::unbounded_channel();
        let release = Arc::new(Semaphore::new(0));
        let held = {
            let release = release.clone();
            move || {
                let release = release.clone();
                let sender = sender.clone();
                async move {
                    sender.send(()).unwrap();
                    release.acquire().await.unwrap().forget();
                    StatusCode::NO_CONTENT
                }
            }
        };
        let router = Router::new()
            .route("/held", get(held.clone()))
            .route("/uploads/progress", get(held.clone()))
            .route("/uploads/frames", post(held))
            .route("/head", get(head))
            .route("/time", get(lww::time))
            .route("/ack", post(lww::ack))
            .route_layer(middleware::from_fn_with_state(app.clone(), authorize))
            .route("/notify", get(lww::notify))
            .with_state(app.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        // Building a client loads the platform root store, which takes hundreds of milliseconds on macOS.
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap();
        Self {
            _root: root,
            app,
            shutdown,
            task,
            client,
            endpoint,
            library: device.library_id,
            token: device.token,
            entered,
            release,
        }
    }

    fn request(&self, path: &str) -> tokio::task::JoinHandle<reqwest::Response> {
        let request = if path == "/uploads/frames" {
            self.client.post(format!("{}{path}", self.endpoint))
        } else {
            self.client.get(format!("{}{path}", self.endpoint))
        };
        let request = request
            .bearer_auth(&self.token)
            .header("x-risu-library", &self.library);
        tokio::spawn(async move { request.send().await.unwrap() })
    }

    async fn entered(&mut self) {
        tokio::time::timeout(Duration::from_secs(5), self.entered.recv())
            .await
            .unwrap()
            .unwrap();
    }
}

#[tokio::test]
async fn queued_bulk_bodies_do_not_block_control_requests() {
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let _serial = lww::tests::NOTIFY_SOCKETS.lock().await;
    let mut fixture = Fixture::new(4).await;
    let cpu = fixture
        .app
        .materializers
        .clone()
        .acquire_owned()
        .await
        .unwrap();
    let mut jobs = Vec::new();
    for _ in 0..4 {
        jobs.push(fixture.request("/uploads/frames"));
        fixture.entered().await;
    }
    for _ in 0..8 {
        jobs.push(fixture.request("/uploads/frames"));
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    let controls = async {
        assert_eq!(fixture.request("/head").await.unwrap().status(), 200);
        assert_eq!(fixture.request("/time").await.unwrap().status(), 200);
        let response = fixture
            .client
            .post(format!("{}/ack", fixture.endpoint))
            .bearer_auth(&fixture.token)
            .header("x-risu-library", &fixture.library)
            .body(r#"{"seq":"0"}"#)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 204);
        let mut request = format!("{}/notify", fixture.endpoint.replace("http://", "ws://"))
            .into_client_request()
            .unwrap();
        request.headers_mut().insert(
            "authorization",
            format!("Bearer {}", fixture.token).parse().unwrap(),
        );
        request
            .headers_mut()
            .insert("x-risu-library", fixture.library.parse().unwrap());
        let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
        assert_eq!(
            socket.next().await.unwrap().unwrap().into_text().unwrap(),
            r#"{"type":"seq","seq":"0"}"#
        );
        socket.close(None).await.unwrap();
        while socket.next().await.is_some() {}
    };
    let completed = tokio::time::timeout(Duration::from_secs(5), controls)
        .await
        .is_ok();
    drop(cpu);
    fixture.release.add_permits(12);
    for job in jobs {
        assert_eq!(job.await.unwrap().status(), 204);
    }
    assert!(completed, "control requests waited behind bulk capacity");
}

#[tokio::test]
async fn progress_waits_have_no_request_count_cap() {
    let mut fixture = Fixture::new(4).await;
    let jobs = (0..18)
        .map(|_| fixture.request("/uploads/progress"))
        .collect::<Vec<_>>();
    let entered = tokio::time::timeout(Duration::from_secs(1), async {
        for _ in 0..18 {
            fixture.entered.recv().await.unwrap();
        }
    })
    .await
    .is_ok();
    fixture.release.add_permits(18);
    for job in jobs {
        assert_eq!(job.await.unwrap().status(), 204);
    }
    assert!(
        entered,
        "progress requests waited behind a request count cap"
    );
}

#[tokio::test]
async fn one_device_can_receive_while_two_upload_requests_are_active() {
    let mut fixture = Fixture::new(4).await;
    let first = fixture.request("/uploads/frames");
    let second = fixture.request("/uploads/frames");
    fixture.entered().await;
    fixture.entered().await;
    let response = fixture.request("/head").await.unwrap();
    fixture.release.add_permits(2);
    assert_eq!(first.await.unwrap().status(), 204);
    assert_eq!(second.await.unwrap().status(), 204);
    assert_eq!(response.status(), 200);
}

#[tokio::test]
async fn exhausted_buffer_capacity_waits_then_completes() {
    let mut fixture = Fixture::new(1).await;
    let first = fixture.request("/uploads/frames");
    fixture.entered().await;
    let mut second = fixture.request("/uploads/frames");
    let early = tokio::time::timeout(Duration::from_millis(200), &mut second).await;
    fixture.release.add_permits(2);
    assert_eq!(first.await.unwrap().status(), 204);
    assert!(
        early.is_err(),
        "capacity exhaustion must wait instead of returning a refusal"
    );
    assert_eq!(second.await.unwrap().status(), 204);
}

#[tokio::test]
async fn shutdown_releases_queued_requests_without_cancelling_active_work() {
    let mut fixture = Fixture::new(1).await;
    let active = fixture.request("/uploads/frames");
    fixture.entered().await;
    let mut queued = fixture.request("/uploads/frames");
    assert!(
        tokio::time::timeout(Duration::from_millis(200), &mut queued)
            .await
            .is_err()
    );
    assert_eq!(fixture.app.workload.status().unwrap().active_requests, 1);
    fixture.shutdown.send_replace(true);
    let response = queued.await.unwrap();
    assert_eq!(response.status(), 503);
    assert_eq!(
        response.json::<serde_json::Value>().await.unwrap()["error"],
        "server-updating"
    );
    fixture.release.add_permits(1);
    assert_eq!(active.await.unwrap().status(), 204);
}

#[tokio::test]
async fn cancelled_capacity_waits_release_their_queue_position() {
    let fixture = Fixture::new(4).await;
    for capacity in [
        &fixture.app.buffers,
        &fixture.app.materializers,
        &fixture.app.media_slots,
    ] {
        let count = capacity.available_permits();
        let held = capacity.acquire_many(count as u32).await.unwrap();
        assert!(tokio::time::timeout(
            Duration::from_millis(10),
            fixture.app.wait_slot(capacity.clone())
        )
        .await
        .is_err());
        drop(held);
        let permit = tokio::time::timeout(
            Duration::from_secs(1),
            fixture.app.wait_slot(capacity.clone()),
        )
        .await
        .unwrap()
        .unwrap();
        drop(permit);
        assert_eq!(capacity.available_permits(), count);
    }
}

#[tokio::test]
async fn a_registration_revoked_while_queued_cannot_run_after_admission() {
    let mut fixture = Fixture::new(1).await;
    let active = fixture.request("/uploads/frames");
    fixture.entered().await;
    let mut queued = fixture.request("/uploads/frames");
    assert!(
        tokio::time::timeout(Duration::from_millis(200), &mut queued)
            .await
            .is_err()
    );
    let device = fixture
        .app
        .store
        .authenticate(&fixture.library, &fixture.token)
        .unwrap();
    fixture.app.store.revoke_device(&device.id).unwrap();
    fixture.release.add_permits(1);
    assert_eq!(active.await.unwrap().status(), 204);
    assert_eq!(queued.await.unwrap().status(), 401);
}

fn media_grant(fixture: &Fixture, bytes: &[u8], expired: bool) -> (String, String) {
    use risunest_sync_connect::media::{MediaClaims, MediaObject, MediaRequest, MediaSigner};
    let store = &fixture.app.store;
    let device = store
        .authenticate(&fixture.library, &fixture.token)
        .unwrap();
    let hash = risunest_sync_wire::hash(bytes);
    store.put_object(&device, &hash, bytes).unwrap();
    let object = MediaObject {
        hash: hash.clone(),
        size: (bytes.len() as u64).into(),
        mime: "image/png".into(),
    };
    let request = MediaRequest {
        refresh_url: MediaSigner::new(&[4; 32])
            .unwrap()
            .refresh_url("http://127.0.0.1:12345", &object)
            .unwrap(),
        object,
    };
    let head = store.head().unwrap();
    let token = if expired {
        let db = rusqlite::Connection::open(fixture._root.path().join("metadata.sqlite")).unwrap();
        let key: Vec<u8> = db
            .query_row(
                "SELECT key FROM media_secret WHERE singleton=1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        MediaSigner::new(&key)
            .unwrap()
            .sign_access(&MediaClaims {
                library_id: head.library_id,
                epoch: head.epoch,
                device_id: device.id,
                request,
                expires: 0.into(),
            })
            .unwrap()
    } else {
        store
            .media_access(&device, &head.epoch, &[request])
            .unwrap()
            .remove(0)
            .token
    };
    (token, hash)
}

#[tokio::test]
async fn media_without_a_body_does_not_wait_for_stream_capacity() {
    let fixture = Fixture::new(4).await;
    let (token, hash) = media_grant(&fixture, b"synthetic media", false);
    let (expired, _) = media_grant(&fixture, b"synthetic media", true);
    let (empty, _) = media_grant(&fixture, b"", false);
    let _held = fixture.app.media_slots.acquire_many(16).await.unwrap();
    let head = tokio::time::timeout(
        Duration::from_secs(1),
        media(
            State(fixture.app.clone()),
            Path(token.clone()),
            axum::http::Method::HEAD,
            HeaderMap::new(),
        ),
    )
    .await
    .expect("HEAD waited for stream capacity")
    .unwrap();
    assert_eq!(head.status(), 200);
    assert_eq!(head.headers()["content-length"], "15");
    drop(head);
    for (token, header_value, status) in [
        ("invalid".to_owned(), None, 403),
        (expired, None, 307),
        (empty, None, 200),
        (
            token.clone(),
            Some(("if-none-match", format!("\"{hash}\""))),
            304,
        ),
        (token, Some(("range", "bytes=999-".into())), 416),
    ] {
        let mut headers = HeaderMap::new();
        if let Some((name, value)) = header_value {
            headers.insert(name, value.parse().unwrap());
        }
        let response = tokio::time::timeout(
            Duration::from_secs(1),
            media(
                State(fixture.app.clone()),
                Path(token),
                axum::http::Method::GET,
                headers,
            ),
        )
        .await
        .expect("bodyless media waited for stream capacity")
        .unwrap_or_else(IntoResponse::into_response);
        assert_eq!(response.status().as_u16(), status);
    }
}

#[tokio::test]
async fn media_streams_hold_capacity_until_eof_or_drop_and_recheck_revocation() {
    let fixture = Fixture::new(4).await;
    let bytes = vec![7; 128 * 1024];
    let (token, _) = media_grant(&fixture, &bytes, false);
    let held = fixture.app.media_slots.acquire_many(15).await.unwrap();
    let response = media(
        State(fixture.app.clone()),
        Path(token.clone()),
        axum::http::Method::GET,
        HeaderMap::new(),
    )
    .await
    .unwrap();
    assert_eq!(fixture.app.media_slots.available_permits(), 0);
    assert_eq!(
        axum::body::to_bytes(response.into_body(), bytes.len())
            .await
            .unwrap()
            .as_ref(),
        bytes
    );
    assert_eq!(fixture.app.media_slots.available_permits(), 1);
    let response = media(
        State(fixture.app.clone()),
        Path(token.clone()),
        axum::http::Method::GET,
        HeaderMap::new(),
    )
    .await
    .unwrap();
    assert_eq!(fixture.app.media_slots.available_permits(), 0);
    drop(response);
    assert_eq!(fixture.app.media_slots.available_permits(), 1);
    let last = fixture.app.media_slots.acquire().await.unwrap();
    let pending = media(
        State(fixture.app.clone()),
        Path(token),
        axum::http::Method::GET,
        HeaderMap::new(),
    );
    tokio::pin!(pending);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut pending)
            .await
            .is_err()
    );
    let device = fixture
        .app
        .store
        .authenticate(&fixture.library, &fixture.token)
        .unwrap();
    fixture.app.store.revoke_device(&device.id).unwrap();
    drop(last);
    assert_eq!(pending.await.unwrap_err().status, 403);
    drop(held);
    assert_eq!(fixture.app.media_slots.available_permits(), 16);
}

#[tokio::test]
async fn shutdown_wakes_media_and_materializer_waits() {
    let fixture = Fixture::new(4).await;
    let (token, _) = media_grant(&fixture, b"synthetic media", false);
    let _streams = fixture.app.media_slots.acquire_many(16).await.unwrap();
    let _cpu = fixture.app.materializers.acquire().await.unwrap();
    let stream = media(
        State(fixture.app.clone()),
        Path(token),
        axum::http::Method::GET,
        HeaderMap::new(),
    );
    let materializer = fixture.app.wait_slot(fixture.app.materializers.clone());
    tokio::pin!(stream, materializer);
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut stream)
            .await
            .is_err()
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(10), &mut materializer)
            .await
            .is_err()
    );
    fixture.shutdown.send_replace(true);
    for error in [stream.await.unwrap_err(), materializer.await.unwrap_err()] {
        assert_eq!((error.status, error.code), (503, "server-updating"));
    }
}
