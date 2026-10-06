mod common;
use common::*;
use futures_util::{SinkExt, StreamExt};
use reqwest::{Client, RequestBuilder, StatusCode};
use risunest_sync_server::{
    http,
    store::{DeviceCredential, Store},
    workload::Workload,
};
use risunest_sync_wire::{
    canonical,
    lww::{CancelOperationRequest, OperationReceipt},
    transfer::{self, Frame},
};
use std::{sync::Arc, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{client::IntoClientRequest, Message},
    MaybeTlsStream, WebSocketStream,
};

type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;
struct Server {
    base: String,
    store: Arc<Store>,
    client: Client,
    workload: Workload,
    device: DeviceCredential,
    task: tokio::task::JoinHandle<()>,
    shutdown: tokio::sync::watch::Sender<bool>,
    _dir: tempfile::TempDir,
}
impl Server {
    async fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::init(dir.path()).unwrap());
        let device = store.add_device().unwrap();
        let workload = Workload::new();
        let (shutdown, signal) = tokio::sync::watch::channel(false);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let router = http::router_with_shutdown(store.clone(), workload.clone(), signal);
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        Self {
            base,
            store,
            client: Client::builder().no_proxy().build().unwrap(),
            workload,
            device,
            task,
            shutdown,
            _dir: dir,
        }
    }
    fn auth(&self, request: RequestBuilder) -> RequestBuilder {
        request
            .bearer_auth(&self.device.token)
            .header("x-risu-library", &self.device.library_id)
    }
    async fn socket(&self) -> Socket {
        let mut request = format!("{}/notify", self.base.replace("http://", "ws://"))
            .into_client_request()
            .unwrap();
        request.headers_mut().insert(
            "authorization",
            format!("Bearer {}", self.device.token).parse().unwrap(),
        );
        request
            .headers_mut()
            .insert("x-risu-library", self.device.library_id.parse().unwrap());
        connect_async(request).await.unwrap().0
    }
    fn actor(&self) -> risunest_sync_server::store::Device {
        self.store
            .authenticate(&self.device.library_id, &self.device.token)
            .unwrap()
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn next(socket: &mut Socket) -> Message {
    tokio::time::timeout(Duration::from_secs(3), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn websocket_initial_changed_sequence_and_rfc_ping_echo() {
    let server = Server::start().await;
    let url = format!("{}/notify", server.base.replace("http://", "ws://"));
    let denied = connect_async(&url).await.err().unwrap();
    assert!(
        matches!(denied, tokio_tungstenite::tungstenite::Error::Http(response) if response.status() == 401)
    );
    let mut socket = server.socket().await;
    assert_eq!(
        next(&mut socket).await.into_text().unwrap(),
        r#"{"type":"seq","seq":"0"}"#
    );
    socket
        .send(Message::Ping(b"synthetic heartbeat".to_vec().into()))
        .await
        .unwrap();
    assert_eq!(
        next(&mut socket).await,
        Message::Pong(b"synthetic heartbeat".to_vec().into())
    );
    server
        .store
        .push(
            &server.actor(),
            &request(
                &server.store,
                WRITER_A,
                "changed",
                vec![inline("a", WRITER_A, 1, "a")],
            ),
        )
        .unwrap();
    assert_eq!(
        next(&mut socket).await.into_text().unwrap(),
        r#"{"type":"seq","seq":"1"}"#
    );
    socket.close(None).await.unwrap();
}

#[tokio::test]
async fn notification_connections_have_no_count_cap_and_remain_drainable() {
    let server = Server::start().await;
    let mut sockets = Vec::new();
    for _ in 0..32 {
        let mut socket = server.socket().await;
        assert!(next(&mut socket).await.is_text());
        sockets.push(socket);
    }
    assert!(server.workload.status().unwrap().drained);
    let mut request = format!("{}/notify", server.base.replace("http://", "ws://"))
        .into_client_request()
        .unwrap();
    request.headers_mut().insert(
        "authorization",
        format!("Bearer {}", server.device.token).parse().unwrap(),
    );
    request
        .headers_mut()
        .insert("x-risu-library", server.device.library_id.parse().unwrap());
    let (mut extra, _) = tokio::time::timeout(Duration::from_secs(5), connect_async(request))
        .await
        .expect("notification connection waited behind existing sockets")
        .unwrap();
    assert!(next(&mut extra).await.is_text());
    sockets.push(extra);
    let response = server
        .auth(
            server
                .client
                .get(format!("{}/changes?after=0", server.base)),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    server.shutdown.send(true).unwrap();
    for mut socket in sockets {
        assert!(next(&mut socket).await.is_close());
    }
}

#[tokio::test]
async fn revocation_closes_a_socket_without_occupying_request_slots() {
    let server = Server::start().await;
    let mut socket = server.socket().await;
    next(&mut socket).await;
    server
        .store
        .revoke_device(&server.device.device_id)
        .unwrap();
    assert!(next(&mut socket).await.is_close());
    assert!(server.workload.status().unwrap().drained);
}

#[tokio::test]
async fn delayed_post_after_absent_lookup_and_terminal_cancel_cannot_accept() {
    let server = Server::start().await;
    let req = request(
        &server.store,
        WRITER_A,
        "delayed",
        vec![inline("a", WRITER_A, 1, "a")],
    );
    let body = canonical::encode(&req).unwrap();
    let mut socket = tokio::net::TcpStream::connect(server.base.strip_prefix("http://").unwrap())
        .await
        .unwrap();
    let headers = format!("POST /push HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {}\r\nX-Risu-Library: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", server.device.token, server.device.library_id, body.len());
    socket.write_all(headers.as_bytes()).await.unwrap();
    socket.write_all(&body[..body.len() / 2]).await.unwrap();
    let lookup = server
        .auth(
            server
                .client
                .get(format!("{}/operations/delayed", server.base)),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(lookup.status(), StatusCode::NOT_FOUND);
    let cancel = server
        .auth(
            server
                .client
                .post(format!("{}/operations/delayed/cancel", server.base)),
        )
        .json(&CancelOperationRequest {
            body_digest: req.digest().unwrap(),
        })
        .send()
        .await
        .unwrap();
    assert!(
        matches!(cancel.json::<OperationReceipt>().await.unwrap(), OperationReceipt::Rejected { error, .. } if error == "operation-cancelled")
    );
    socket.write_all(&body[body.len() / 2..]).await.unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(3), socket.read_to_end(&mut response))
        .await
        .unwrap()
        .unwrap();
    assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 409"));
    assert_eq!(server.store.head().unwrap().seq.as_str(), "0");
}

#[tokio::test]
async fn absent_lookup_alone_does_not_prevent_a_delayed_post_accepting() {
    let server = Server::start().await;
    let req = request(
        &server.store,
        WRITER_A,
        "delayed",
        vec![inline("a", WRITER_A, 1, "a")],
    );
    assert_eq!(
        server
            .auth(
                server
                    .client
                    .get(format!("{}/operations/delayed", server.base))
            )
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    let response = server
        .auth(server.client.post(format!("{}/push", server.base)))
        .json(&req)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let proof = server
        .auth(
            server
                .client
                .post(format!("{}/operations/delayed/cancel", server.base)),
        )
        .json(&CancelOperationRequest {
            body_digest: req.digest().unwrap(),
        })
        .send()
        .await
        .unwrap();
    assert!(matches!(
        proof.json::<OperationReceipt>().await.unwrap(),
        OperationReceipt::Accepted { .. }
    ));
}
#[tokio::test]
async fn missing_negotiation_repairs_a_lost_body_before_retention() {
    let server = Server::start().await;
    let device = server
        .store
        .authenticate(&server.device.library_id, &server.device.token)
        .unwrap();
    let body = vec![b'x'; 128 * 1024];
    let digest = risunest_sync_wire::hash(&body);
    server.store.put_object(&device, &digest, &body).unwrap();
    let path = server
        .store
        .data_path()
        .join("objects")
        .join(&digest[..2])
        .join(&digest);
    std::fs::remove_file(path).unwrap();
    let response: serde_json::Value = server
        .auth(
            server
                .client
                .post(format!("{}/objects/missing", server.base)),
        )
        .json(&serde_json::json!([{"hash":digest,"size":body.len().to_string()}]))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(response["missing"], serde_json::json!([digest]));
    server
        .auth(
            server
                .client
                .post(format!("{}/uploads/frames", server.base)),
        )
        .body(transfer::encode(&[Frame::Full(body.clone())]).unwrap())
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    server
        .store
        .retain_objects(
            &device,
            &server.store.head().unwrap().epoch,
            &[risunest_sync_server::store::ObjectIdentity {
                hash: digest.clone(),
                size: Some((body.len() as u64).into()),
            }],
        )
        .unwrap();
    assert_eq!(server.store.get_object(&digest).unwrap(), body);
    server.task.abort();
}
