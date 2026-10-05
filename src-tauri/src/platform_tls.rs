//! TLS for native HTTP and WebSocket connections. Servers are verified with
//! the platform's own trust decision, so a CA the user installed counts
//! wherever the platform honors it: native-tls on Windows, macOS, Linux and
//! iOS, and the Android certificate verifier on Android, which follows the
//! app's network security config (system and user CAs).

/// An async client builder whose servers are verified by the platform.
pub(crate) fn client_builder() -> reqwest::ClientBuilder {
    apply(reqwest::Client::builder())
}

/// Applies platform verification to a builder another component created.
pub(crate) fn apply(builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
    #[cfg(not(target_os = "android"))]
    {
        builder.use_native_tls()
    }
    #[cfg(target_os = "android")]
    {
        match android::http_config() {
            Some(config) => builder.use_preconfigured_tls(config),
            None => builder.use_preconfigured_tls(android::VerifierUnavailable),
        }
    }
}

/// A blocking client builder whose servers are verified by the platform.
pub(crate) fn blocking_client_builder() -> reqwest::blocking::ClientBuilder {
    let builder = reqwest::blocking::Client::builder();
    #[cfg(not(target_os = "android"))]
    {
        builder.use_native_tls()
    }
    #[cfg(target_os = "android")]
    {
        match android::http_config() {
            Some(config) => builder.use_preconfigured_tls(config),
            None => builder.use_preconfigured_tls(android::VerifierUnavailable),
        }
    }
}

/// The connector for `wss` connections, or `None` when TLS is unavailable.
pub(crate) fn websocket_connector() -> Option<tokio_tungstenite::Connector> {
    #[cfg(not(target_os = "android"))]
    {
        native_tls::TlsConnector::new()
            .ok()
            .map(tokio_tungstenite::Connector::NativeTls)
    }
    #[cfg(target_os = "android")]
    {
        android::websocket_config()
            .map(|config| tokio_tungstenite::Connector::Rustls(std::sync::Arc::new(config)))
    }
}

#[cfg(target_os = "android")]
mod android {
    use rustls_platform_verifier::BuilderVerifierExt;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    static VERIFIER_READY: AtomicBool = AtomicBool::new(false);

    /// A TLS value reqwest does not recognize, so `build` fails instead of a
    /// handshake reaching a verifier that has no JVM to call.
    pub(super) struct VerifierUnavailable;

    #[no_mangle]
    pub extern "system" fn Java_io_github_rsyumi_risunest_PlatformTls_initialize(
        mut env: jni::JNIEnv,
        _class: jni::objects::JClass,
        context: jni::objects::JObject,
    ) {
        match rustls_platform_verifier::android::init_with_env(&mut env, context) {
            Ok(()) => VERIFIER_READY.store(true, Ordering::Release),
            Err(_) => {
                let _ = env.exception_clear();
            }
        }
    }

    fn config(alpn_protocols: Vec<Vec<u8>>) -> Option<rustls::ClientConfig> {
        if !VERIFIER_READY.load(Ordering::Acquire) {
            return None;
        }
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let mut config = rustls::ClientConfig::builder_with_provider(provider)
            .with_protocol_versions(rustls::ALL_VERSIONS)
            .ok()?
            .with_platform_verifier()
            .ok()?
            .with_no_client_auth();
        config.alpn_protocols = alpn_protocols;
        Some(config)
    }

    /// Offers the protocols reqwest offers with its own rustls configuration.
    pub(super) fn http_config() -> Option<rustls::ClientConfig> {
        config(vec![b"h2".to_vec(), b"http/1.1".to_vec()])
    }

    /// WebSocket upgrades need HTTP/1.1, so no protocol is offered.
    pub(super) fn websocket_config() -> Option<rustls::ClientConfig> {
        config(Vec::new())
    }
}

#[cfg(all(test, not(target_os = "android")))]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::{
            atomic::{AtomicU32, Ordering},
            Arc,
        },
        thread::JoinHandle,
        time::Duration,
    };

    /// A `localhost` endpoint with a self-signed certificate no platform
    /// store holds. It answers `connections` TLS connections; a handshake the
    /// client refuses ends that connection.
    struct Endpoint {
        port: u16,
        certificate: reqwest::Certificate,
        server: JoinHandle<()>,
    }

    fn endpoint(connections: usize) -> Endpoint {
        // OpenSSL looks an issuer up by name, so no two certificates share one.
        static ISSUED: AtomicU32 = AtomicU32::new(0);
        let mut params = rcgen::CertificateParams::new(vec!["localhost".to_owned()]).unwrap();
        params.distinguished_name.push(
            rcgen::DnType::CommonName,
            format!(
                "risunest platform tls endpoint {}",
                ISSUED.fetch_add(1, Ordering::Relaxed)
            ),
        );
        let signing_key = rcgen::KeyPair::generate().unwrap();
        let cert = params.self_signed(&signing_key).unwrap();
        let certificate = reqwest::Certificate::from_pem(cert.pem().as_bytes()).unwrap();
        let config = Arc::new(
            rustls::ServerConfig::builder_with_provider(Arc::new(
                rustls::crypto::ring::default_provider(),
            ))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![cert.der().clone()],
                rustls::pki_types::PrivatePkcs8KeyDer::from(signing_key.serialize_der()).into(),
            )
            .unwrap(),
        );
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            for _ in 0..connections {
                let (socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut stream = rustls::StreamOwned::new(
                    rustls::ServerConnection::new(config.clone()).unwrap(),
                    socket,
                );
                let mut head = Vec::new();
                let mut byte = [0];
                while !head.ends_with(b"\r\n\r\n") {
                    if stream.read_exact(&mut byte).is_err() {
                        break;
                    }
                    head.push(byte[0]);
                }
                if head.ends_with(b"\r\n\r\n") {
                    let _ = stream.write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                    );
                    let _ = stream.flush();
                }
            }
        });
        Endpoint {
            port,
            certificate,
            server,
        }
    }

    fn platform_store_refused(error: &(dyn std::error::Error + 'static)) -> bool {
        let mut source = Some(error);
        while let Some(error) = source {
            if error.is::<native_tls::Error>() {
                return true;
            }
            source = error.source();
        }
        false
    }

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    #[test]
    fn async_clients_and_the_http_plugin_verify_with_the_platform_store() {
        let endpoint = endpoint(2);
        let url = format!("https://localhost:{}/", endpoint.port);
        let runtime = runtime();
        for builder in [client_builder(), apply(reqwest::ClientBuilder::new())] {
            let client = builder.no_proxy().build().unwrap();
            let error = runtime.block_on(client.get(&url).send()).unwrap_err();
            assert!(error.is_connect(), "{error:?}");
            assert!(platform_store_refused(&error), "{error:?}");
        }
        endpoint.server.join().unwrap();
    }

    #[test]
    fn blocking_clients_verify_with_the_platform_store() {
        let endpoint = endpoint(1);
        let client = blocking_client_builder().no_proxy().build().unwrap();
        let error = client
            .get(format!("https://localhost:{}/", endpoint.port))
            .send()
            .unwrap_err();
        assert!(platform_store_refused(&error), "{error:?}");
        endpoint.server.join().unwrap();
    }

    #[test]
    fn a_trusted_certificate_still_has_to_name_the_host() {
        let endpoint = endpoint(2);
        let client = client_builder()
            .no_proxy()
            .add_root_certificate(endpoint.certificate.clone())
            .build()
            .unwrap();
        let runtime = runtime();
        let response = runtime
            .block_on(client.get(format!("https://localhost:{}/", endpoint.port)).send())
            .unwrap();
        assert_eq!(runtime.block_on(response.text()).unwrap(), "ok");
        let error = runtime
            .block_on(client.get(format!("https://127.0.0.1:{}/", endpoint.port)).send())
            .unwrap_err();
        assert!(platform_store_refused(&error), "{error:?}");
        endpoint.server.join().unwrap();
    }

    #[test]
    fn websocket_connections_verify_with_the_platform_store() {
        let endpoint = endpoint(1);
        let connector = websocket_connector().unwrap();
        assert!(matches!(connector, tokio_tungstenite::Connector::NativeTls(_)));
        let error = runtime()
            .block_on(tokio_tungstenite::connect_async_tls_with_config(
                format!("wss://localhost:{}/notify", endpoint.port),
                None,
                false,
                Some(connector),
            ))
            .unwrap_err();
        assert!(
            matches!(
                error,
                tokio_tungstenite::tungstenite::Error::Tls(
                    tokio_tungstenite::tungstenite::error::TlsError::Native(_)
                )
            ),
            "{error:?}"
        );
        endpoint.server.join().unwrap();
    }
}
