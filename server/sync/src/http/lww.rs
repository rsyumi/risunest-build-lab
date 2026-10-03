use super::*;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use risunest_sync_wire::{
    lww::{
        AckRequest, CancelOperationRequest, NewDeviceClaimRequest, PushRequest, SeqNotification,
    },
    stamp::DecimalU64,
    unit::UnitKey,
};

#[cfg(not(test))]
const NOTIFY_CHECK: Duration = Duration::from_secs(1);
#[cfg(test)]
const NOTIFY_CHECK: Duration = Duration::from_millis(20);

pub(super) async fn time(State(app): State<App>) -> Result<Response> {
    blocking(move || Ok(Json(app.store.time_sample()?).into_response())).await
}
pub(super) async fn claim_writer(
    State(app): State<App>,
    Extension(device): Extension<Device>,
    body: Bytes,
) -> Result<Response> {
    let request: NewDeviceClaimRequest = canonical::decode(&body, MAX_METADATA_BYTES)?;
    blocking(
        move || Ok(Json(app.store.claim_new_device_writer(&device, &request)?).into_response()),
    )
    .await
}
pub(super) async fn push(
    State(app): State<App>,
    Extension(device): Extension<Device>,
    body: Bytes,
) -> Result<Response> {
    let request: PushRequest = canonical::decode(&body, MAX_METADATA_BYTES)?;
    blocking(move || Ok(Json(app.store.push(&device, &request)?).into_response())).await
}
pub(super) async fn operation(
    State(app): State<App>,
    Extension(device): Extension<Device>,
    Path(id): Path<String>,
) -> Result<Response> {
    blocking(move || Ok(Json(app.store.operation(&device, &id)?).into_response())).await
}
pub(super) async fn cancel_operation(
    State(app): State<App>,
    Extension(device): Extension<Device>,
    Path(id): Path<String>,
    body: Bytes,
) -> Result<Response> {
    let request: CancelOperationRequest = canonical::decode(&body, MAX_METADATA_BYTES)?;
    blocking(move || Ok(Json(app.store.cancel_operation(&device, &id, &request)?).into_response()))
        .await
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ChangesQuery {
    after: DecimalU64,
    limit: Option<DecimalU64>,
}
fn limit(value: Option<DecimalU64>) -> Result<usize> {
    let value = value.unwrap_or(128.into()).0;
    if value == 0 || value > 1024 {
        return Err(Error::new("invalid-limit", 400));
    }
    Ok(value as usize)
}
pub(super) async fn changes(
    State(app): State<App>,
    Extension(device): Extension<Device>,
    Query(query): Query<ChangesQuery>,
) -> Result<Response> {
    let count = limit(query.limit)?;
    blocking(move || Ok(Json(app.store.changes(&device, query.after, count)?).into_response()))
        .await
}
pub(super) async fn create_pin(
    State(app): State<App>,
    Extension(device): Extension<Device>,
) -> Result<Response> {
    blocking(move || Ok(Json(app.store.create_state_pin(&device)?).into_response())).await
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct StateQuery {
    pin: String,
    after_key: Option<UnitKey>,
    limit: Option<DecimalU64>,
}
pub(super) async fn state(
    State(app): State<App>,
    Extension(device): Extension<Device>,
    Query(query): Query<StateQuery>,
) -> Result<Response> {
    let count = limit(query.limit)?;
    blocking(move || {
        Ok(Json(
            app.store
                .state_page(&device, &query.pin, query.after_key.as_ref(), count)?,
        )
        .into_response())
    })
    .await
}
pub(super) async fn release_pin(
    State(app): State<App>,
    Extension(device): Extension<Device>,
    Path(id): Path<String>,
) -> Result<Response> {
    blocking(move || app.store.release_state_pin(&device, &id)).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}
pub(super) async fn ack(
    State(app): State<App>,
    Extension(device): Extension<Device>,
    body: Bytes,
) -> Result<Response> {
    let request: AckRequest = canonical::decode(&body, MAX_METADATA_BYTES)?;
    blocking(move || app.store.acknowledge(&device, &request)).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

pub(super) async fn notify(
    State(app): State<App>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Result<Response> {
    let token = header(&headers, "authorization")
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(Error::new("unauthorized", 401))?
        .to_owned();
    let library = header(&headers, "x-risu-library")
        .ok_or(Error::new("unauthorized", 401))?
        .to_owned();
    let store = app.store.clone();
    let device = blocking(move || store.authenticate(&library, &token)).await?;
    if *app.shutdown.borrow() || app.workload.status()?.state != "open" {
        return Err(Error::new("server-updating", 503));
    }
    let permit = app
        .notice_slots
        .clone()
        .try_acquire_owned()
        .map_err(|_| Error::new("server-busy", 429))?;
    let mut response = ws
        .max_message_size(1024)
        .max_frame_size(1024)
        .write_buffer_size(0)
        .max_write_buffer_size(4096)
        .on_upgrade(move |socket| notify_socket(app, device, socket, permit))
        .into_response();
    response
        .headers_mut()
        .insert("cache-control", "no-store".parse().unwrap());
    Ok(response)
}

async fn notify_socket(
    mut app: App,
    device: Device,
    mut socket: WebSocket,
    _permit: tokio::sync::OwnedSemaphorePermit,
) {
    let mut announced = app.store.head_announcements();
    let mut last = None;
    // Head moves and revocations are announced, so the database is read only
    // then; the timer rechecks the in-memory workload state.
    let mut stale = true;
    let mut check = tokio::time::interval(NOTIFY_CHECK);
    loop {
        if *app.shutdown.borrow() || !app.workload.status().is_ok_and(|s| s.state == "open") {
            break;
        }
        if std::mem::take(&mut stale) {
            #[cfg(test)]
            tests::HEAD_READS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let store = app.store.clone();
            let actor = device.clone();
            let Ok(head) = blocking(move || store.device_head(&actor)).await else {
                break;
            };
            let Ok(seq) = DecimalU64::try_from(head.seq.as_str().to_owned()) else {
                break;
            };
            if last != Some(seq) {
                let Ok(body) = serde_json::to_string(&SeqNotification::Seq { seq }) else {
                    break;
                };
                if !send_notice(&mut socket, &mut app.shutdown, Message::Text(body.into())).await {
                    break;
                }
                last = Some(seq);
            }
        }
        tokio::select! {
            message = socket.recv() => match message {
                Some(Ok(Message::Ping(bytes))) => {
                    if !send_notice(&mut socket, &mut app.shutdown, Message::Pong(bytes)).await { break; }
                }
                Some(Ok(Message::Pong(_))) => (),
                _ => break,
            },
            changed = announced.changed() => {
                if changed.is_err() { break; }
                stale = true;
            }
            _ = check.tick() => {
                #[cfg(test)]
                tests::CHECKS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            _ = stopped(&mut app.shutdown) => break,
        }
    }
    let _ = tokio::time::timeout(Duration::from_secs(1), socket.send(Message::Close(None))).await;
}

async fn stopped(shutdown: &mut tokio::sync::watch::Receiver<bool>) {
    if shutdown.has_changed().is_err() && !*shutdown.borrow() {
        std::future::pending::<()>().await;
    } else {
        let _ = shutdown.wait_for(|stopped| *stopped).await;
    }
}

async fn send_notice(
    socket: &mut WebSocket,
    shutdown: &mut tokio::sync::watch::Receiver<bool>,
    message: Message,
) -> bool {
    tokio::select! {
        result = tokio::time::timeout(Duration::from_secs(10), socket.send(message)) => matches!(result, Ok(Ok(()))),
        _ = stopped(shutdown) => false,
    }
}

#[cfg(test)]
pub(super) mod tests {
    use futures_util::{SinkExt, StreamExt};
    use risunest_sync_wire::{
        lww::{PushRequest, UnitChange},
        stamp::Stamp,
        unit::{UnitKey, UnitValue},
    };
    use std::{
        sync::{
            atomic::{AtomicU64, Ordering},
            Arc,
        },
        time::Duration,
    };
    use tokio_tungstenite::tungstenite::{client::IntoClientRequest, Message};

    pub(crate) static HEAD_READS: AtomicU64 = AtomicU64::new(0);
    pub(crate) static CHECKS: AtomicU64 = AtomicU64::new(0);

    type Socket = tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >;
    async fn next(socket: &mut Socket) -> Message {
        tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
    }

    #[tokio::test]
    async fn an_idle_socket_reads_the_head_only_when_it_is_announced() {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(crate::store::Store::init(dir.path()).unwrap());
        let credential = store.add_device().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router = super::super::router(store.clone());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let mut request = format!("ws://{address}/notify")
            .into_client_request()
            .unwrap();
        request.headers_mut().insert(
            "authorization",
            format!("Bearer {}", credential.token).parse().unwrap(),
        );
        request
            .headers_mut()
            .insert("x-risu-library", credential.library_id.parse().unwrap());
        let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
        assert_eq!(
            next(&mut socket).await.into_text().unwrap(),
            r#"{"type":"seq","seq":"0"}"#
        );
        let reads = HEAD_READS.load(Ordering::SeqCst);
        let checks = CHECKS.load(Ordering::SeqCst);
        tokio::time::timeout(Duration::from_secs(10), async {
            while CHECKS.load(Ordering::SeqCst) < checks + 5 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(HEAD_READS.load(Ordering::SeqCst), reads);
        let actor = store
            .authenticate(&credential.library_id, &credential.token)
            .unwrap();
        let writer = "00000000-0000-4000-8000-000000000001";
        store
            .push(
                &actor,
                &PushRequest {
                    library_id: credential.library_id.clone(),
                    writer_id: writer.into(),
                    operation_id: "announced".into(),
                    changes: vec![UnitChange {
                        key: UnitKey::new(&["root", "key"]).unwrap(),
                        stamp: Stamp {
                            physical_ms: 1.into(),
                            logical: 0,
                            writer_id: writer.into(),
                        },
                        value: UnitValue::inline(br#""value""#).unwrap(),
                    }],
                },
            )
            .unwrap();
        assert_eq!(
            next(&mut socket).await.into_text().unwrap(),
            r#"{"type":"seq","seq":"1"}"#
        );
        assert_eq!(HEAD_READS.load(Ordering::SeqCst), reads + 1);
        socket.send(Message::Close(None)).await.unwrap();
        server.abort();
    }
}
