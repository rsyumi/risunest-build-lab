use super::*;
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::Arc,
};

fn response(stream: &mut impl Write, body: &str, status: &str) {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    stream.flush().unwrap();
}
fn request(stream: &mut impl Read) -> String {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        bytes.push(byte[0]);
        assert!(bytes.len() < 8192);
    }
    String::from_utf8(bytes).unwrap()
}
fn tls_identity(device: &str) -> (String, reqwest::Certificate, std::thread::JoinHandle<()>) {
    tls_identity_requests(device, 1)
}

fn tls_identity_requests(
    device: &str,
    requests: usize,
) -> (String, reqwest::Certificate, std::thread::JoinHandle<()>) {
    // Every rcgen certificate carries the same subject and no authority key
    // identifier, so a client holding two of them cannot tell which key signed
    // an endpoint and OpenSSL rejects the second one. Name each one apart.
    static ISSUED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let mut params = rcgen::CertificateParams::new(vec!["localhost".to_owned()]).unwrap();
    params.distinguished_name.push(
        rcgen::DnType::CommonName,
        format!(
            "risunest synthetic endpoint {}",
            ISSUED.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ),
    );
    let signing_key = rcgen::KeyPair::generate().unwrap();
    let cert = params.self_signed(&signing_key).unwrap();
    let certificate = reqwest::Certificate::from_der(cert.der()).unwrap();
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![cert.der().clone()],
        rustls::pki_types::PrivatePkcs8KeyDer::from(signing_key.serialize_der()).into(),
    )
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!(
        "https://localhost:{}",
        listener.local_addr().unwrap().port()
    );
    let device = device.to_owned();
    let task = std::thread::spawn(move || {
        let config = Arc::new(config);
        for _ in 0..requests {
            let (socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut stream = rustls::StreamOwned::new(
                rustls::ServerConnection::new(config.clone()).unwrap(),
                socket,
            );
            let input = request(&mut stream);
            assert!(input.starts_with("GET /session ") || input.starts_with("GET /head "));
            assert!(input.to_lowercase().contains("authorization: bearer "));
            let head = serde_json::to_value(risunest_sync_wire::RemoteHead {
                head_id: "a".repeat(64),
                ..risunest_sync_wire::RemoteHead::genesis("library".into(), "epoch".into()).unwrap()
            })
            .unwrap();
            let body = if input.starts_with("GET /session ") {
                serde_json::json!({"head":head,"deviceId":device,"protocolId":risunest_sync_wire::PROTOCOL_ID}).to_string()
            } else {
                head.to_string()
            };
            response(&mut stream, &body, "200 OK");
        }
    });
    (endpoint, certificate, task)
}

#[test]
fn mid_cycle_failure_rediscovers_and_verifies_the_changed_endpoint() {
    let (old_endpoint, old_certificate, old_server) = tls_identity("device");
    let (new_endpoint, new_certificate, new_server) = tls_identity_requests("device", 2);
    let mut directory = directory();
    let envelope =
        risunest_sync_connect::seal_endpoint(&directory.uuid, &directory.key, &new_endpoint)
            .unwrap();
    let (registry, registry_task) = directory_server(envelope);
    directory.base_url = registry;
    let config = ServerConfig {
        endpoint: old_endpoint,
        library_id: "library".into(),
        device_id: "device".into(),
        token: "a".repeat(64),
        directory: Some(directory),
    };
    let mut client = ServerClient::new(config).unwrap();
    client.http = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .add_root_certificate(old_certificate)
        .add_root_certificate(new_certificate)
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    assert!(client.resolve_identity().is_ok());
    old_server.join().unwrap();
    let head = client.head().unwrap();
    assert_eq!(head.library_id, "library");
    assert_eq!(client.config().endpoint, new_endpoint);
    registry_task.join().unwrap();
    new_server.join().unwrap();
}

#[test]
fn authenticated_identity_rejection_does_not_try_registry_fallback() {
    let identity = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", identity.server_addr());
    let identity_task = std::thread::spawn(move || {
        let request = identity.recv().unwrap();
        assert_eq!(request.url(), "/session");
        request
            .respond(
                tiny_http::Response::from_string(r#"{"error":"unauthorized"}"#)
                    .with_status_code(401),
            )
            .unwrap();
    });
    let registry = TcpListener::bind("127.0.0.1:0").unwrap();
    registry.set_nonblocking(true).unwrap();
    let mut directory = directory();
    directory.base_url = format!("http://{}", registry.local_addr().unwrap());
    let client = ServerClient::new(ServerConfig {
        endpoint,
        library_id: "library".into(),
        device_id: "device".into(),
        token: "a".repeat(64),
        directory: Some(directory),
    })
    .unwrap();
    let error = client.resolve_identity().unwrap_err();
    assert_eq!(error.status, 401);
    assert!(matches!(
        registry.accept(),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
    ));
    identity_task.join().unwrap();
}
fn directory_server(body: String) -> (String, std::thread::JoinHandle<()>) {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", server.server_addr());
    let task = std::thread::spawn(move || {
        let request = server.recv_timeout(Duration::from_secs(10)).unwrap().expect("registry lookup");
        assert_eq!(request.method(), &tiny_http::Method::Get);
        assert!(request.url().starts_with("/endpoints/"));
        assert!(!request.headers().iter().any(|header| {
            header.field.equiv("authorization") || header.field.equiv("x-risu-library")
        }));
        request.respond(tiny_http::Response::from_string(body)).unwrap();
    });
    (endpoint, task)
}

struct ResponseServer {
    endpoint: String,
    requests: Arc<std::sync::atomic::AtomicUsize>,
    server: Arc<tiny_http::Server>,
    task: Option<std::thread::JoinHandle<()>>,
}
impl ResponseServer {
    fn new(body: String, status: u16) -> Self {
        let server = Arc::new(tiny_http::Server::http("127.0.0.1:0").unwrap());
        let endpoint = format!("http://{}", server.server_addr());
        let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let serving = server.clone();
        let count = requests.clone();
        let task = std::thread::spawn(move || {
            while let Ok(request) = serving.recv() {
                count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if status == 200 {
                    assert_eq!(request.method(), &tiny_http::Method::Get);
                    assert!(request.url().starts_with("/endpoints/"));
                    assert!(!request.headers().iter().any(|header| {
                        header.field.equiv("authorization") || header.field.equiv("x-risu-library")
                    }));
                }
                request.respond(tiny_http::Response::from_string(body.clone()).with_status_code(status)).unwrap();
            }
        });
        Self { endpoint, requests, server, task: Some(task) }
    }
    fn requests(&self) -> usize { self.requests.load(std::sync::atomic::Ordering::Relaxed) }
}
impl Drop for ResponseServer {
    fn drop(&mut self) {
        self.server.unblock();
        self.task.take().unwrap().join().unwrap();
    }
}
fn directory() -> risunest_sync_connect::Directory {
    risunest_sync_connect::Directory {
        base_url: "https://registry.example".into(),
        uuid: "12345678-1234-4234-9234-123456789abc".into(),
        key: "A".repeat(43),
    }
}
#[test]
fn registry_retry_admission_increases_to_a_cap_and_is_scoped_to_the_record() {
    let mut retries = DirectoryRetries::default();
    let mut now = Instant::now();
    for delay in [30, 60, 120, 240, 300, 300] {
        assert!(retries.admit("record-a", now));
        assert!(!retries.admit("record-a", now));
        now += Duration::from_secs(delay);
        assert!(!retries.admit("record-a", now - Duration::from_millis(1)));
    }
    assert!(retries.admit("record-b", now));
    retries.0.remove("record-a");
    assert!(retries.admit("record-a", now));
    assert!(retries.admit("record-a", now + Duration::from_secs(30)));
}

#[test]
fn offline_clients_share_registry_admission() {
    let registry = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let mut directory = directory();
    directory.base_url = format!("http://{}", registry.server_addr());
    let mut config = tests::config("http://127.0.0.1:1");
    config.directory = Some(directory);
    let first = ServerClient::new(config.clone()).unwrap();
    let task = std::thread::spawn(move || first.resolve_identity());
    let request = registry.recv_timeout(Duration::from_secs(5)).unwrap().expect("registry lookup");
    request.respond(tiny_http::Response::empty(503)).unwrap();
    assert_eq!(task.join().unwrap().unwrap_err().code, "directory-response-error");
    // A new client is what another transport lane or the next scheduled attempt creates.
    let second = ServerClient::new(config.clone()).unwrap();
    assert_eq!(second.resolve_identity().unwrap_err().code, "server-unreachable");
    assert!(registry.try_recv().unwrap().is_none());
    reset_directory_retry(&config);
    let task = std::thread::spawn(move || second.resolve_identity());
    let request = registry.recv_timeout(Duration::from_secs(5)).unwrap().expect("lookup after recovery reset");
    request.respond(tiny_http::Response::empty(503)).unwrap();
    assert_eq!(task.join().unwrap().unwrap_err().code, "directory-response-error");
}
#[test]
fn configuring_through_directory_persists_only_the_verified_endpoint() {
    use crate::server_sync::{binding, credentials::StoredConfig, lww_client::OperationLog, lww_tests::local};

    for device in ["device", "wrong-device"] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let input = request(&mut stream);
            assert!(input.starts_with("GET /session "));
            assert!(input.to_lowercase().contains("authorization: bearer "));
            let head = risunest_sync_wire::RemoteHead::genesis("library".into(), "epoch".into()).unwrap();
            let body = serde_json::json!({
                "head": head,
                "deviceId": device,
                "protocolId": risunest_sync_wire::PROTOCOL_ID,
            }).to_string();
            response(&mut stream, &body, "200 OK");
        });
        let mut directory = directory();
        let envelope = risunest_sync_connect::seal_endpoint(&directory.uuid, &directory.key, &endpoint).unwrap();
        let (registry, registry_task) = directory_server(envelope);
        directory.base_url = registry;
        let retired = ResponseServer::new(String::new(), 503);
        let mut config = tests::config(&retired.endpoint);
        config.directory = Some(directory);
        let (root, store) = local();
        let previous = StoredConfig::persist(root.path(), &config).unwrap();
        let log = OperationLog::open(root.path()).unwrap();
        log.save_config("candidate", &previous).unwrap();
        let result = binding::configure(&store, config.clone());
        let stored = log.config("candidate").unwrap().unwrap();
        if device == "device" {
            assert!(result.is_ok(), "{}", result.unwrap_err().code);
            let saved = stored.resolve(root.path()).unwrap();
            assert_eq!(saved.endpoint, endpoint);
            assert_eq!(saved.library_id, config.library_id);
            assert_eq!(saved.device_id, config.device_id);
            assert_eq!(saved.token, config.token);
            assert_eq!(serde_json::to_value(saved.directory).unwrap(), serde_json::to_value(config.directory).unwrap());
        } else {
            assert_eq!(result.unwrap_err().code, "device-identity-mismatch");
            assert_eq!(serde_json::to_value(stored).unwrap(), serde_json::to_value(previous).unwrap());
        }
        registry_task.join().unwrap();
        server.join().unwrap();
    }
}

#[test]
fn separate_receive_and_ack_clients_recover_a_stored_old_endpoint() {
    use axum::response::IntoResponse;
    use crate::server_sync::{
        credentials::StoredConfig,
        lww_tests::{drain_publications, header, local, save, LocalServerFixture},
    };

    let lost_reply = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let lose_first_reply = lost_reply.clone();
    let server = LocalServerFixture::with_router(|router| {
        router.layer(axum::middleware::from_fn(
            move |request: axum::extract::Request, next: axum::middleware::Next| {
                let lose_first_reply = lose_first_reply.clone();
                async move {
                    let ack = request.uri().path() == "/ack";
                    let response = next.run(request).await;
                    if ack && response.status().is_success()
                        && lose_first_reply.swap(false, std::sync::atomic::Ordering::Relaxed)
                    {
                        axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response()
                    } else {
                        response
                    }
                }
            },
        ))
    });
    let (_source_root, mut source) = local();
    let (_target_root, mut target) = local();
    let sender = server.client(&source);
    let receiver = server.client(&target);
    let mut config = receiver.client.config();
    drop(receiver);
    let mut directory = directory();
    let envelope = risunest_sync_connect::seal_endpoint(&directory.uuid, &directory.key, &server.endpoint).unwrap();
    let registry = ResponseServer::new(envelope, 200);
    directory.base_url = registry.endpoint.clone();
    config.directory = Some(directory);
    let retired = ResponseServer::new(String::new(), 503);
    let stored = StoredConfig::persist(target.repository_root(), &config).unwrap();
    target.server_save_config(&stored).unwrap();

    for language in ["ja", "de"] {
        save(&mut source, &["root", "language"], serde_json::json!(language));
        drain_publications(&sender, &mut source, &[]).unwrap();
        let request = header(&target);
        let page = crate::server_sync::commands::with_lww_client(&mut target, None, |core, store| {
            core.client.retry_budget = tests::no_sleep_budget();
            core.receive_page(store, &request)
        }).unwrap();
        target.lww_stage_receive(&page).unwrap();
        target.lww_apply_receive(&crate::persistent_store::lww::ApplyReceive {
            header: page.header.clone(),
            generating: vec![],
        }).unwrap();
        target.lww_finish_receive(&page.header).unwrap();
        if language == "ja" {
            let mut outdated = stored.clone();
            outdated.endpoint = retired.endpoint.clone();
            target.server_save_config(&outdated).unwrap();
        }
        // The production ACK command opens another client from the stored registration.
        crate::server_sync::commands::with_lww_client(&mut target, None, |core, store| {
            core.client.retry_budget = tests::no_sleep_budget();
            core.finish_receive(store, &page.header)
        }).unwrap();
        let db = rusqlite::Connection::open(server.root().join("metadata.sqlite")).unwrap();
        let ack: String = db.query_row("SELECT ack FROM devices WHERE id=?1", [&config.device_id], |row| row.get(0)).unwrap();
        assert_eq!(ack, page.progress.cursor.0.to_string());
    }
    assert!(!lost_reply.load(std::sync::atomic::Ordering::Relaxed));
    save(&mut target, &["root", "language"], serde_json::json!("fr"));
    let request = header(&target);
    assert!(crate::server_sync::commands::with_lww_client(&mut target, None, |core, store| {
        core.push(store, &request, &[])
    }).unwrap().is_some());
    assert_eq!(registry.requests(), 1);
}

fn seed_binding_server(server: &crate::server_sync::lww_tests::LocalServerFixture) {
    use crate::server_sync::lww_tests::{drain_publications, local, save};
    let (_root, mut source) = local();
    save(&mut source, &["root", "language"], serde_json::json!("ja"));
    drain_publications(&server.client(&source), &mut source, &[]).unwrap();
}

fn install_binding_config(store: &crate::persistent_store::PersistentStore, config: &crate::server_sync::credentials::StoredConfig) {
    let log = crate::server_sync::lww_client::OperationLog::open(store.repository_root()).unwrap();
    store.server_save_config(config).unwrap();
    log.save_config("active", config).unwrap();
    for slot in ["active", "selected"] {
        let mut target: serde_json::Value = log.verified("bindings", slot).unwrap();
        target["config"] = serde_json::to_value(config).unwrap();
        log.0.execute("UPDATE bindings SET body=?1 WHERE id=?2", rusqlite::params![target.to_string(), slot]).unwrap();
    }
}

fn assert_binding_config(store: &crate::persistent_store::PersistentStore, expected: &crate::server_sync::credentials::StoredConfig) {
    let log = crate::server_sync::lww_client::OperationLog::open(store.repository_root()).unwrap();
    let target: serde_json::Value = log.verified("bindings", "active").unwrap();
    let expected = serde_json::to_value(expected).unwrap();
    assert_eq!(serde_json::to_value(store.server_stored_config().unwrap().unwrap()).unwrap(), expected);
    assert_eq!(serde_json::to_value(log.config("active").unwrap().unwrap()).unwrap(), expected);
    assert_eq!(target["config"], expected);
}

#[test]
fn recovered_binding_endpoints_survive_reactivation_without_replacing_credentials() {
    use crate::server_sync::{binding, commands::with_lww_client, credentials::StoredConfig, lww_client::OperationLog, lww_tests::{header, local, LocalServerFixture}};
    for during_activation in [true, false] {
        let server = LocalServerFixture::new();
        seed_binding_server(&server);
        let (root, mut store) = local();
        let (mut config, _) = server.candidate(&store);
        let mut directory = directory();
        let registry = ResponseServer::new(risunest_sync_connect::seal_endpoint(&directory.uuid, &directory.key, &server.endpoint).unwrap(), 200);
        directory.base_url = registry.endpoint.clone();
        config.directory = Some(directory);
        let expected = StoredConfig::persist(root.path(), &config).unwrap();
        OperationLog::open(root.path()).unwrap().save_config("candidate", &expected).unwrap();
        binding::first_binding_cycle(&mut store, Arc::new(TestIoCounters::default()), |_| {}).unwrap();
        let retired = ResponseServer::new(String::new(), 503);
        let mut outdated = expected.clone();
        outdated.endpoint = retired.endpoint.clone();
        install_binding_config(&store, &outdated);
        drop(store);
        let mut store = crate::persistent_store::PersistentStore::open(root.path()).unwrap();
        if during_activation {
            let request = header(&store);
            binding::activate(&mut store, &request, None).unwrap();
        } else {
            let error = with_lww_client(&mut store, None, |core, _| {
                core.client.resolve_identity()?;
                Err::<(), _>(SyncError::new("synthetic-operation-failed", 409))
            }).unwrap_err();
            assert_eq!(error.code, "synthetic-operation-failed");
        }
        assert_binding_config(&store, &expected);
        drop(store);
        let mut store = crate::persistent_store::PersistentStore::open(root.path()).unwrap();
        let request = header(&store);
        binding::activate(&mut store, &request, None).unwrap();
        assert_binding_config(&store, &expected);
        assert_eq!(registry.requests(), 1);
    }
}

#[test]
fn a_successful_push_survives_endpoint_persistence_failure() {
    use crate::server_sync::{binding, commands::with_lww_client, credentials::StoredConfig, lww_client::OperationLog, lww_tests::{header, local, save, LocalServerFixture}};
    let server = LocalServerFixture::new();
    seed_binding_server(&server);
    let (root, mut store) = local();
    let (mut config, _) = server.candidate(&store);
    let mut directory = directory();
    let registry = ResponseServer::new(risunest_sync_connect::seal_endpoint(&directory.uuid, &directory.key, &server.endpoint).unwrap(), 200);
    directory.base_url = registry.endpoint.clone();
    config.directory = Some(directory);
    let expected = StoredConfig::persist(root.path(), &config).unwrap();
    OperationLog::open(root.path()).unwrap().save_config("candidate", &expected).unwrap();
    binding::first_binding_cycle(&mut store, Arc::new(TestIoCounters::default()), |_| {}).unwrap();
    let retired = ResponseServer::new(String::new(), 503);
    let mut outdated = expected.clone();
    outdated.endpoint = retired.endpoint.clone();
    install_binding_config(&store, &outdated);
    save(&mut store, &["root", "language"], serde_json::json!("fr"));
    let request = header(&store);
    with_lww_client(&mut store, None, |core, store| {
        core.client.retry_budget = tests::no_sleep_budget();
        core.log.0.execute_batch(
            "CREATE TEMP TRIGGER refuse_endpoint_save BEFORE UPDATE ON configurations
             BEGIN SELECT RAISE(FAIL, 'synthetic endpoint persistence failure'); END;"
        )?;
        core.push(store, &request, &[])
    }).unwrap().expect("the successful publication returns its receipt");
    assert_binding_config(&store, &outdated);
    with_lww_client(&mut store, None, |core, _| {
        core.client.retry_budget = tests::no_sleep_budget();
        core.client.resolve_identity()
    }).unwrap();
    assert_binding_config(&store, &expected);
}

#[test]
fn late_endpoint_recovery_cannot_overwrite_a_newer_endpoint_or_credential() {
    use crate::server_sync::{binding, credentials::StoredConfig, lww_client::LwwClient, lww_tests::{local, LocalServerFixture}};
    let server = LocalServerFixture::new();
    seed_binding_server(&server);
    let (root, mut store) = local();
    server.prepare_binding_candidate(&store);
    binding::first_binding_cycle(&mut store, Arc::new(TestIoCounters::default()), |_| {}).unwrap();
    let expected = store.server_stored_config().unwrap().unwrap();
    for changed_credential in [false, true] {
        install_binding_config(&store, &expected);
        let mut resolved = expected.resolve(root.path()).unwrap();
        resolved.endpoint = server.endpoint.replace("127.0.0.1", "localhost");
        let mut late = LwwClient::new(root.path(), resolved).unwrap();
        late.access = Some(expected.clone());
        late.client.resolve_identity().unwrap();
        let newer = if changed_credential {
            StoredConfig::persist(root.path(), &expected.resolve(root.path()).unwrap()).unwrap()
        } else {
            let mut newer = expected.clone();
            newer.endpoint = format!("{}/", server.endpoint);
            newer
        };
        install_binding_config(&store, &newer);
        binding::persist_resolved_endpoint(&store, &late).unwrap();
        assert_binding_config(&store, &newer);
    }
}

#[test]
fn native_directory_recovery_verifies_identity_before_changing_endpoint() {
    for device in ["device", "wrong-device"] {
        let (endpoint, certificate, server) = tls_identity(device);
        let mut directory = directory();
        let envelope =
            risunest_sync_connect::seal_endpoint(&directory.uuid, &directory.key, &endpoint)
                .unwrap();
        let (registry, registry_task) = directory_server(envelope);
        directory.base_url = registry;
        let config = ServerConfig {
            endpoint: "http://127.0.0.1:1".into(),
            library_id: "library".into(),
            device_id: "device".into(),
            token: "a".repeat(64),
            directory: Some(directory),
        };
        let mut client = ServerClient::new(config).unwrap();
        client.http = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .add_root_certificate(certificate)
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let result = client.resolve_identity();
        if device == "device" {
            assert!(result.is_ok(), "{}", result.unwrap_err().code);
            assert_eq!(client.config().endpoint, endpoint);
        } else {
            assert_eq!(result.unwrap_err().code, "device-identity-mismatch");
            assert_eq!(client.config().endpoint, "http://127.0.0.1:1");
        }
        registry_task.join().unwrap();
        server.join().unwrap();
    }
}
#[test]
fn native_directory_rejects_tampered_and_oversized_envelopes_without_mutation() {
    for body in [
        "A".repeat(100),
        "A".repeat(risunest_sync_connect::MAX_ENVELOPE_TEXT + 1),
    ] {
        let (registry, task) = directory_server(body);
        let mut directory = directory();
        directory.base_url = registry;
        let config = ServerConfig {
            endpoint: "http://127.0.0.1:1".into(),
            library_id: "library".into(),
            device_id: "device".into(),
            token: "a".repeat(64),
            directory: Some(directory),
        };
        let client = ServerClient::new(config).unwrap();
        assert!(client.resolve_identity().is_err());
        assert_eq!(client.config().endpoint, "http://127.0.0.1:1");
        task.join().unwrap();
    }
}

#[test]
fn synthetic_https_fixture_accepts_the_native_client() {
    let (endpoint, certificate, server) = tls_identity("device");
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .add_root_certificate(certificate)
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let reply = client
        .get(format!("{endpoint}/session"))
        .bearer_auth("a".repeat(64))
        .send();
    assert!(reply.is_ok(), "synthetic fixture TLS: {:?}", reply.err());
    server.join().unwrap();
}

#[test]
fn native_verified_cached_endpoint_does_not_read_directory() {
    let (endpoint, certificate, server) = tls_identity("device");
    let mut directory = directory();
    directory.base_url = "http://127.0.0.1:1".into();
    let config = ServerConfig {
        endpoint: endpoint.clone(),
        library_id: "library".into(),
        device_id: "device".into(),
        token: "a".repeat(64),
        directory: Some(directory),
    };
    let mut client = ServerClient::new(config).unwrap();
    client.http = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .add_root_certificate(certificate)
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    assert!(client.resolve_identity().is_ok());
    assert_eq!(client.config().endpoint, endpoint);
    server.join().unwrap();
}
