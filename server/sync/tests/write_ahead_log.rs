mod common;
use common::{inline, request, WRITER_A, WRITER_B};
use futures_util::StreamExt;
use reqwest::{Client, RequestBuilder, StatusCode};
use risunest_sync_connect::media::{MediaAccess, MediaObject, MediaRequest, MediaSigner};
use risunest_sync_server::{
    http,
    management::{discovery::Discovery, Management},
    store::{DeviceCredential, Store},
};
use risunest_sync_wire::{
    hash,
    lww::{ChangesPage, PushReceipt, StatePage, StatePin},
    transfer::{self, Frame},
    RemoteHead,
};
use std::{sync::Arc, time::Duration};
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};

/// Turns a lost response into a failure; loopback replies arrive far sooner.
const BOUND: Duration = Duration::from_secs(60);

struct Library {
    base: String,
    client: Client,
    store: Arc<Store>,
}

impl Library {
    fn auth(&self, request: RequestBuilder, device: &DeviceCredential) -> RequestBuilder {
        request
            .bearer_auth(&device.token)
            .header("x-risu-library", &device.library_id)
    }
    async fn get(&self, device: &DeviceCredential, path: &str) -> reqwest::Response {
        let response = self
            .auth(self.client.get(format!("{}{path}", self.base)), device)
            .send()
            .await
            .unwrap();
        assert!(
            response.status().is_success(),
            "{path}: {}",
            response.status()
        );
        response
    }
    async fn post(
        &self,
        device: &DeviceCredential,
        path: &str,
        body: &serde_json::Value,
    ) -> reqwest::Response {
        let response = self
            .auth(self.client.post(format!("{}{path}", self.base)), device)
            .json(body)
            .send()
            .await
            .unwrap();
        assert!(
            response.status().is_success(),
            "{path}: {}",
            response.status()
        );
        response
    }
    async fn push(&self, device: &DeviceCredential, writer: &str, operation: &str, key: &str) {
        let publication = request(
            &self.store,
            writer,
            operation,
            vec![inline(key, writer, 1, operation)],
        );
        let receipt: PushReceipt = self
            .post(device, "/push", &serde_json::to_value(publication).unwrap())
            .await
            .json()
            .await
            .unwrap();
        assert_eq!(receipt.operation_id, operation);
    }
    async fn receive(&self, device: &DeviceCredential) -> ChangesPage {
        self.get(device, "/changes?after=0&limit=1024")
            .await
            .json()
            .await
            .unwrap()
    }
}

/// Joins, publishes, receives, reads the library state, moves assets and reads
/// the management session the way two installations and the manager do.
async fn use_library(library: &Library, a: &DeviceCredential, b: &DeviceCredential) {
    for (device, writer) in [(a, WRITER_A), (b, WRITER_B)] {
        library.get(device, "/session").await;
        library
            .post(
                device,
                "/session/writer",
                &serde_json::json!({"writerId": writer}),
            )
            .await;
        library.get(device, "/session/claim-writer").await;
        library.get(device, "/time").await;
    }
    let head: RemoteHead = library.get(a, "/head").await.json().await.unwrap();
    library.push(a, WRITER_A, "joined", "joined").await;
    let received = library.receive(b).await;
    assert_eq!(received.items.len(), 1);
    let pin: StatePin = library
        .auth(
            library.client.post(format!("{}/state/pins", library.base)),
            b,
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let page: StatePage = library
        .get(b, &format!("/state?pin={}", pin.pin_id))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
    let released = library
        .auth(
            library
                .client
                .delete(format!("{}/state/pins/{}", library.base, pin.pin_id)),
            b,
        )
        .send()
        .await
        .unwrap();
    assert_eq!(released.status(), StatusCode::NO_CONTENT);
    library
        .post(b, "/ack", &serde_json::json!({"seq": received.through_seq}))
        .await;

    let small = b"synthetic inline asset".to_vec();
    let large = vec![b'f'; 256 * 1024];
    let frames =
        transfer::encode(&[Frame::Full(small.clone()), Frame::Full(large.clone())]).unwrap();
    let uploaded = library
        .auth(
            library
                .client
                .post(format!("{}/uploads/frames", library.base)),
            a,
        )
        .body(frames)
        .send()
        .await
        .unwrap();
    assert_eq!(uploaded.status(), StatusCode::NO_CONTENT);
    let identities = [&small, &large]
        .map(|bytes| serde_json::json!({"hash": hash(bytes), "size": bytes.len().to_string()}));
    let missing: serde_json::Value = library
        .post(b, "/objects/missing", &serde_json::json!(identities))
        .await
        .json()
        .await
        .unwrap();
    assert_eq!(missing["missing"], serde_json::json!([]));
    let targets =
        [&small, &large].map(|bytes| serde_json::json!({"target": hash(bytes), "bases": []}));
    library
        .post(b, "/objects/transfer", &serde_json::json!(targets))
        .await
        .bytes()
        .await
        .unwrap();
    for bytes in [&small, &large] {
        let fetched = library
            .get(b, &format!("/objects/{}", hash(bytes)))
            .await
            .bytes()
            .await
            .unwrap();
        assert_eq!(fetched.as_ref(), bytes.as_slice());
    }
    let object = MediaObject {
        hash: hash(&large),
        size: (large.len() as u64).into(),
        mime: "image/png".into(),
    };
    let refresh = MediaSigner::new(&[7; 32])
        .unwrap()
        .refresh_url(&library.base, &object)
        .unwrap();
    let grants: Vec<MediaAccess> = library
        .post(
            b,
            "/media/access",
            &serde_json::json!({"epoch": head.epoch, "requests": [MediaRequest {object, refresh_url: refresh}]}),
        )
        .await
        .json()
        .await
        .unwrap();
    let media = library
        .client
        .get(format!("{}/media/{}", library.base, grants[0].token))
        .send()
        .await
        .unwrap();
    assert_eq!(media.bytes().await.unwrap().len(), large.len());
    library
        .post(
            b,
            "/objects/retention",
            &serde_json::json!({"epoch": head.epoch, "objects": [identities[1]]}),
        )
        .await;
    library
        .get(b, &format!("/objects/retention?epoch={}", head.epoch))
        .await;

    let manager = Discovery::load(library.store.data_path()).unwrap();
    let status: serde_json::Value = library
        .client
        .get(format!("http://{}/status", manager.address))
        .bearer_auth(&manager.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["devices"].as_array().unwrap().len(), 2);
}

#[test]
fn a_served_library_folds_its_write_ahead_log_back_after_concurrent_traffic() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::init(dir.path()).unwrap());
    let a = store.add_device().unwrap();
    let b = store.add_device().unwrap();
    // The server runs on its own threads, so publications and reads from the
    // two installations reach the store at the same time.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let origin = listener.local_addr().unwrap();
    let (router, management) = runtime.block_on(async {
        let management = Management::start(store.clone(), origin).await.unwrap();
        (http::router(store.clone()), management)
    });
    let server = runtime.spawn(async move {
        let listener = tokio::net::TcpListener::from_std(listener).unwrap();
        axum::serve(listener, router).await.unwrap()
    });
    let library = Library {
        base: format!("http://{origin}"),
        client: Client::builder().no_proxy().timeout(BOUND).build().unwrap(),
        store: store.clone(),
    };
    let client = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    client.block_on(async {
        let mut notify = format!("ws://{origin}/notify")
            .into_client_request()
            .unwrap();
        notify.headers_mut().insert(
            "authorization",
            format!("Bearer {}", b.token).parse().unwrap(),
        );
        notify
            .headers_mut()
            .insert("x-risu-library", b.library_id.parse().unwrap());
        let (mut socket, _) = tokio_tungstenite::connect_async(notify).await.unwrap();
        let first = tokio::time::timeout(BOUND, socket.next()).await.unwrap();
        assert!(matches!(first, Some(Ok(Message::Text(_)))));

        use_library(&library, &a, &b).await;

        // One installation publishes while the other keeps receiving.
        let publisher = async {
            for index in 0..150 {
                let name = format!("burst-{index}");
                library.push(&a, WRITER_A, &name, &name).await;
            }
        };
        let receiver = async {
            for _ in 0..150 {
                library.get(&b, "/head").await;
                library.receive(&b).await;
            }
        };
        tokio::join!(publisher, receiver);
        socket.close(None).await.unwrap();
    });

    client.block_on(library.push(&b, WRITER_B, "after", "after"));
    let result = store.maintain().unwrap();
    let checkpoint = result.wal_checkpoint;
    assert!(
        !checkpoint.incomplete(),
        "busy={} log={} checkpointed={}",
        checkpoint.busy,
        checkpoint.log_frames,
        checkpoint.checkpointed_frames
    );
    let wal = store.data_path().join("metadata.sqlite-wal");
    assert_eq!(std::fs::metadata(wal).unwrap().len(), 0);
    runtime.block_on(management.close());
    server.abort();
    runtime.shutdown_background();
}
