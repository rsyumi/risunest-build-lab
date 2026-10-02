use super::*;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use risunest_sync_wire::{
    lww::{
        AckRequest, CancelOperationRequest, NewDeviceClaimRequest, PushRequest, SeqNotification,
    },
    stamp::DecimalU64,
    unit::UnitKey,
};

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
    let mut check = tokio::time::interval(Duration::from_secs(1));
    loop {
        if *app.shutdown.borrow() || !app.workload.status().is_ok_and(|s| s.state == "open") {
            break;
        }
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
        tokio::select! {
            message = socket.recv() => match message {
                Some(Ok(Message::Ping(bytes))) => {
                    if !send_notice(&mut socket, &mut app.shutdown, Message::Pong(bytes)).await { break; }
                }
                Some(Ok(Message::Pong(_))) => (),
                _ => break,
            },
            changed = announced.changed() => if changed.is_err() { break; },
            _ = check.tick() => (),
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
