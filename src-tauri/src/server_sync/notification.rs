use super::{client::ServerConfig, Result, SyncError};
use futures::{SinkExt, StreamExt};
use risunest_sync_wire::lww::SeqNotification;
use std::time::Duration;
use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};

#[cfg(test)]
#[derive(Default, serde::Serialize)]
pub(crate) struct FrameCounters {
    pub text: std::sync::atomic::AtomicU64,
    pub ping_in: std::sync::atomic::AtomicU64,
    pub pong_in: std::sync::atomic::AtomicU64,
    pub ping_out: std::sync::atomic::AtomicU64,
    pub pong_out: std::sync::atomic::AtomicU64,
}
#[cfg(test)]
thread_local! { static COUNTERS: std::cell::RefCell<Option<std::sync::Arc<FrameCounters>>> = const {std::cell::RefCell::new(None)}; static TEST_TIMING: std::cell::Cell<Option<(Duration,Duration)>> = const {std::cell::Cell::new(None)}; }
#[cfg(test)]
pub(crate) fn reset_frame_counters() -> std::sync::Arc<FrameCounters> {
    let counters = std::sync::Arc::new(FrameCounters::default());
    COUNTERS.with(|slot| *slot.borrow_mut() = Some(counters.clone()));
    counters
}

pub(crate) async fn run(
    config: ServerConfig,
    mut notice: impl FnMut(SeqNotification),
    connected: impl Fn(bool),
) -> Result<()> {
    #[cfg(test)]
    let counters = COUNTERS.with(|slot| slot.borrow_mut().take());
    #[cfg(test)]
    let (ping_every, pong_timeout) = TEST_TIMING
        .with(|timing| timing.take())
        .unwrap_or((Duration::from_secs(25), Duration::from_secs(10)));
    #[cfg(not(test))]
    let (ping_every, pong_timeout) = (Duration::from_secs(25), Duration::from_secs(10));
    let mut url = config.validate()?;
    url = url
        .join("notify")
        .map_err(|_| SyncError::new("invalid-request-path", 400))?;
    url.set_scheme(if url.scheme() == "https" { "wss" } else { "ws" })
        .map_err(|_| SyncError::new("invalid-endpoint", 400))?;
    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|_| SyncError::new("invalid-websocket-request", 400))?;
    request.headers_mut().insert(
        "authorization",
        format!("Bearer {}", config.token)
            .parse()
            .map_err(|_| SyncError::new("invalid-device-token", 400))?,
    );
    request.headers_mut().insert(
        "x-risu-library",
        config
            .library_id
            .parse()
            .map_err(|_| SyncError::new("invalid-library", 400))?,
    );
    let secure = url.scheme() == "wss";
    let result = async {
        let connector = if secure {
            crate::platform_tls::websocket_connector()
                .ok_or_else(|| SyncError::new("websocket-unavailable", 503))?
        } else {
            tokio_tungstenite::Connector::Plain
        };
        let (socket, _) = tokio::time::timeout(
            Duration::from_secs(10),
            tokio_tungstenite::connect_async_tls_with_config(request, None, false, Some(connector)),
        )
        .await
        .map_err(|_| SyncError::new("server-timeout", 503))?
        .map_err(|_| SyncError::new("websocket-unavailable", 503))?;
        connected(true);
        run_socket(
            socket,
            &mut notice,
            ping_every,
            pong_timeout,
            #[cfg(test)]
            counters,
        )
        .await
    }
    .await;
    connected(false);
    result
}

async fn run_socket<S>(
    mut socket: tokio_tungstenite::WebSocketStream<S>,
    notice: &mut impl FnMut(SeqNotification),
    ping_every: Duration,
    pong_timeout: Duration,
    #[cfg(test)] counters: Option<std::sync::Arc<FrameCounters>>,
) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let mut interval =
        tokio::time::interval_at(tokio::time::Instant::now() + ping_every, ping_every);
    let mut pending: Option<(Vec<u8>, tokio::time::Instant)> = None;
    let mut serial = 0u64;
    loop {
        let deadline = pending
            .as_ref()
            .map(|p| p.1)
            .unwrap_or(tokio::time::Instant::now() + Duration::from_secs(3600));
        tokio::select! {
            frame=socket.next()=>match frame {
                Some(Ok(Message::Text(body)))=> {
                    #[cfg(test)]
                    if let Some(counters)=&counters {counters.text.fetch_add(1,std::sync::atomic::Ordering::Relaxed);}
                    if body.len()>1024 {return Err(SyncError::new("invalid-notification",502));}
                    let frame=serde_json::from_str::<SeqNotification>(&body).map_err(|_|SyncError::new("invalid-notification",502))?;
                    notice(frame)
                },
                Some(Ok(Message::Pong(bytes)))=> {
                    #[cfg(test)]
                    if let Some(counters)=&counters {counters.pong_in.fetch_add(1,std::sync::atomic::Ordering::Relaxed);}
                    if pending.as_ref().is_some_and(|p|p.0.as_slice()==bytes.as_ref()){pending=None;}
                },
                Some(Ok(Message::Ping(bytes)))=> {
                    #[cfg(test)]
                    if let Some(counters)=&counters {counters.ping_in.fetch_add(1,std::sync::atomic::Ordering::Relaxed);}
                    tokio::time::timeout(pong_timeout,socket.send(Message::Pong(bytes))).await.map_err(|_|SyncError::new("server-timeout",503))?.map_err(|_|SyncError::new("websocket-unavailable",503))?;
                    #[cfg(test)]
                    if let Some(counters)=&counters {counters.pong_out.fetch_add(1,std::sync::atomic::Ordering::Relaxed);}
                },
                _=>return Err(SyncError::new("websocket-unavailable",503)),
            },
            _=interval.tick()=> {
                serial=serial.checked_add(1).ok_or_else(||SyncError::new("notification-overflow",409))?;
                let bytes=serial.to_be_bytes().to_vec();
                tokio::time::timeout(pong_timeout,socket.send(Message::Ping(bytes.clone().into()))).await.map_err(|_|SyncError::new("server-timeout",503))?.map_err(|_|SyncError::new("websocket-unavailable",503))?;
                #[cfg(test)]
                if let Some(counters)=&counters {counters.ping_out.fetch_add(1,std::sync::atomic::Ordering::Relaxed);}
                pending=Some((bytes,tokio::time::Instant::now()+pong_timeout));
            },
            _=tokio::time::sleep_until(deadline),if pending.is_some()=>return Err(SyncError::new("websocket-pong-timeout",503)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{atomic::Ordering, Arc, Mutex};
    fn config(endpoint: String) -> ServerConfig {
        ServerConfig {
            directory: None,
            endpoint,
            library_id: "00000000-0000-4000-8000-000000000001".into(),
            device_id: "00000000-0000-4000-8000-000000000002".into(),
            token: "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into(),
        }
    }
    #[tokio::test]
    async fn native_websocket_uses_headers_delivers_seq_and_answers_real_ping() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut socket = tokio_tungstenite::accept_hdr_async(
                stream,
                |request: &tokio_tungstenite::tungstenite::handshake::server::Request, response| {
                    assert_eq!(request.uri().path(), "/notify");
                    assert!(request.uri().query().is_none());
                    assert_eq!(
                        request.headers()["authorization"],
                        "Bearer aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
                    );
                    assert_eq!(
                        request.headers()["x-risu-library"],
                        "00000000-0000-4000-8000-000000000001"
                    );
                    Ok(response)
                },
            )
            .await
            .unwrap();
            socket
                .send(Message::Text(
                    serde_json::to_string(&SeqNotification::Seq { seq: 1.into() })
                        .unwrap()
                        .into(),
                ))
                .await
                .unwrap();
            socket
                .send(Message::Ping(vec![7, 8, 9].into()))
                .await
                .unwrap();
            loop {
                if let Some(Ok(Message::Pong(bytes))) = socket.next().await {
                    assert_eq!(bytes.as_ref(), &[7, 8, 9]);
                    break;
                }
            }
            socket.close(None).await.unwrap();
        });
        let frames = reset_frame_counters();
        let received = Arc::new(Mutex::new(Vec::new()));
        let copy = received.clone();
        let connected = Arc::new(Mutex::new(Vec::new()));
        let connections = connected.clone();
        let error = run(
            config(endpoint),
            move |frame| copy.lock().unwrap().push(frame),
            move |value| connections.lock().unwrap().push(value),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, "websocket-unavailable");
        server.await.unwrap();
        assert_eq!(
            *received.lock().unwrap(),
            vec![SeqNotification::Seq { seq: 1.into() }]
        );
        assert_eq!(*connected.lock().unwrap(), vec![true, false]);
        assert_eq!(frames.text.load(Ordering::Relaxed), 1);
        assert_eq!(frames.ping_in.load(Ordering::Relaxed), 1);
        assert_eq!(frames.pong_out.load(Ordering::Relaxed), 1);
    }
    #[tokio::test(start_paused = true)]
    async fn a_wrong_nonce_pong_does_not_extend_the_native_deadline() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (client, mut server) = tokio::io::duplex(64);
        let socket = tokio_tungstenite::WebSocketStream::from_raw_socket(
            client,
            tokio_tungstenite::tungstenite::protocol::Role::Client,
            None,
        )
        .await;
        let frames = Arc::new(FrameCounters::default());
        let mut notice = |_| {};
        let run = run_socket(
            socket,
            &mut notice,
            Duration::from_millis(80),
            Duration::from_millis(20),
            Some(frames.clone()),
        );
        tokio::pin!(run);
        assert!(futures::poll!(&mut run).is_pending());
        tokio::time::advance(Duration::from_millis(80)).await;
        assert!(futures::poll!(&mut run).is_pending());
        let mut ping = [0u8; 14];
        server.read_exact(&mut ping).await.unwrap();
        assert_eq!(ping[0], 0x89);
        assert_eq!(ping[1], 0x88);
        tokio::time::advance(Duration::from_millis(10)).await;
        server.write_all(&[0x8a, 1, 99]).await.unwrap();
        assert!(futures::poll!(&mut run).is_pending());
        assert_eq!(frames.pong_in.load(Ordering::Relaxed), 1);
        tokio::time::advance(Duration::from_millis(9)).await;
        assert!(futures::poll!(&mut run).is_pending());
        tokio::time::advance(Duration::from_millis(1)).await;
        let std::task::Poll::Ready(Err(error)) = futures::poll!(&mut run) else {
            panic!("wrong pong changed the original deadline");
        };
        assert_eq!(error.code, "websocket-pong-timeout");
        assert_eq!(frames.ping_out.load(Ordering::Relaxed), 1);
        assert_eq!(frames.pong_in.load(Ordering::Relaxed), 1);
    }
}
