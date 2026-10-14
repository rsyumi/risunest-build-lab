use crate::{source_observer as observed, store::Store, workload::Workload};
use axum::{
    body::{Body, BodyDataStream, Bytes, HttpBody},
    extract::{Request, State},
    http::{Method, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use futures_util::Stream;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    io::{BufRead, Read, Write},
    pin::Pin,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    task::{Context, Poll},
    time::{Duration, Instant},
};
use tokio::sync::{OwnedRwLockReadGuard, OwnedRwLockWriteGuard, RwLock};

const PREFIX: &str = "RISUNEST_SYNC_SOURCE ";
const MAX_COMMAND_BYTES: usize = 64 * 1024 * 1024;
const DRAIN_LIMIT: Duration = Duration::from_secs(10);

#[derive(Clone)]
struct Gate {
    open: Arc<AtomicBool>,
    lock: Arc<RwLock<()>>,
}
impl Gate {
    fn new() -> Self {
        Self {
            open: Arc::new(AtomicBool::new(true)),
            lock: Arc::new(RwLock::new(())),
        }
    }
}
struct ResponseStream {
    stream: BodyDataStream,
    _permit: OwnedRwLockReadGuard<()>,
    _request: observed::Request,
    remaining: Option<u64>,
    finished: bool,
}
impl Stream for ResponseStream {
    type Item = Result<Bytes, axum::Error>;
    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let result = Pin::new(&mut self.stream).poll_next(cx);
        match &result {
            Poll::Ready(None) => self.finished = true,
            Poll::Ready(Some(Ok(bytes))) => {
                if let Some(remaining) = &mut self.remaining {
                    if let Some(left) = remaining.checked_sub(bytes.len() as u64) {
                        *remaining = left;
                    } else {
                        observed::note_violation("response-length-overrun");
                    }
                }
            }
            Poll::Ready(Some(Err(_))) => {
                observed::note_violation("response-body-error");
                self.finished = true;
            }
            Poll::Pending => (),
        }
        result
    }
}
impl Drop for ResponseStream {
    fn drop(&mut self) {
        if !self.finished && self.remaining != Some(0) {
            observed::note_violation("response-body-abandoned");
        }
    }
}
async fn admission(State(gate): State<Gate>, request: Request, next: Next) -> Response {
    if !gate.open.load(Ordering::Acquire) {
        observed::note_violation("request-after-barrier");
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let permit = gate.lock.clone().read_owned().await;
    if !gate.open.load(Ordering::Acquire) {
        observed::note_violation("request-raced-barrier");
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let tracked = observed::request();
    let head = request.method() == Method::HEAD;
    let response = observed::request_context(next.run(request)).await;
    let (parts, body) = response.into_parts();
    let bodyless = head
        || parts.status.is_informational()
        || matches!(
            parts.status,
            StatusCode::NO_CONTENT | StatusCode::NOT_MODIFIED
        );
    let remaining = if bodyless {
        Some(0)
    } else {
        body.size_hint().exact().or_else(|| {
            parts
                .headers
                .get("content-length")
                .and_then(|v| v.to_str().ok()?.parse().ok())
        })
    };
    Response::from_parts(
        parts,
        Body::from_stream(ResponseStream {
            stream: body.into_data_stream(),
            _permit: permit,
            _request: tracked,
            remaining,
            finished: false,
        }),
    )
}

struct Server {
    store: Arc<Store>,
    endpoint: String,
    gate: Gate,
    workload: Workload,
    held: Option<OwnedRwLockWriteGuard<()>>,
    lease: Option<String>,
    shutdown: tokio::sync::watch::Sender<bool>,
    task: tokio::task::JoinHandle<Result<(), std::io::Error>>,
}
impl Server {
    async fn start(root: &std::path::Path) -> Result<Self, String> {
        let store = Arc::new(Store::init(root).map_err(|e| e.code.to_owned())?);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .map_err(|_| "listen-failed")?;
        let endpoint = format!(
            "http://{}",
            listener.local_addr().map_err(|_| "listen-failed")?
        );
        let workload = Workload::with_clock(
            Duration::ZERO,
            Duration::from_secs(3600),
            Arc::new(Instant::now),
        );
        let gate = Gate::new();
        let (shutdown, mut receiver) = tokio::sync::watch::channel(false);
        let router = crate::http::router_with_shutdown(
            store.clone(),
            workload.clone(),
            shutdown.subscribe(),
        )
        .layer(middleware::from_fn_with_state(gate.clone(), admission));
        let task = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(async move {
                    while !*receiver.borrow_and_update() {
                        if receiver.changed().await.is_err() {
                            break;
                        }
                    }
                })
                .await
        });
        Ok(Self {
            store,
            endpoint,
            gate,
            workload,
            held: None,
            lease: None,
            shutdown,
            task,
        })
    }
    async fn barrier(&mut self) -> bool {
        observed::read_barrier::cancel_dangling("read-barrier-drain");
        self.gate.open.store(false, Ordering::Release);
        if self.lease.is_none() {
            match self.workload.acquire() {
                Ok((token, _)) => self.lease = Some(token),
                Err(_) => {
                    observed::note_violation("workload-barrier-failed");
                    return false;
                }
            }
        }
        if self.held.is_none() {
            match tokio::time::timeout(DRAIN_LIMIT, self.gate.lock.clone().write_owned()).await {
                Ok(held) => self.held = Some(held),
                Err(_) => {
                    observed::note_violation("request-drain-timeout");
                    return false;
                }
            }
        }
        let start = Instant::now();
        loop {
            let workload = self.workload.status();
            let source = observed::snapshot(false);
            if workload.is_ok_and(|value| value.drained)
                && source.as_ref().is_none_or(|value| {
                    value.pending_workers == 0
                        && value.pending_requests == 0
                        && value.total.outstanding_readers == 0
                })
            {
                return match self.store.source_observer_body_work() {
                    Ok(work) => {
                        let settled = work.settled();
                        observed::body_work(work);
                        settled
                    }
                    Err(_) => {
                        observed::note_violation("durable-body-status-error");
                        false
                    }
                };
            }
            if start.elapsed() >= DRAIN_LIMIT {
                observed::note_violation("worker-drain-timeout");
                return false;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
    async fn begin(
        &mut self,
        root: &std::path::Path,
        id: String,
        phase: String,
        roles: Vec<observed::Role>,
    ) -> Result<u64, String> {
        if !self.barrier().await {
            return Err("scope-not-settled".into());
        }
        let generation = observed::begin(root, id, phase, roles).map_err(str::to_owned)?;
        if let Some(token) = self.lease.take() {
            self.workload
                .release(&token)
                .map_err(|e| e.code.to_owned())?;
        }
        self.gate.open.store(true, Ordering::Release);
        self.held.take();
        Ok(generation)
    }
    async fn end(&mut self, id: &str) -> Result<observed::Snapshot, String> {
        if observed::snapshot(false).is_none_or(|snapshot| snapshot.scope_id != id) {
            return Err("scope-mismatch".into());
        }
        self.barrier().await;
        observed::snapshot(true).ok_or_else(|| "scope-missing".into())
    }
    async fn stop(mut self) -> Result<Option<observed::Snapshot>, String> {
        if !self.barrier().await {
            return Err("shutdown-not-settled".into());
        }
        self.shutdown.send_replace(true);
        self.held.take();
        tokio::time::timeout(DRAIN_LIMIT, &mut self.task)
            .await
            .map_err(|_| "shutdown-timeout")?
            .map_err(|_| "server-task-failed")?
            .map_err(|_| "server-failed")?;
        Ok(observed::snapshot(true))
    }
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "camelCase", deny_unknown_fields)]
enum Command {
    #[serde(rename_all = "camelCase")]
    Start {
        request_id: String,
        run_id: String,
        expected_source_fingerprint: String,
    },
    #[serde(rename_all = "camelCase")]
    Register {
        request_id: String,
        name: String,
        registration_request_id: String,
    },
    #[serde(rename_all = "camelCase")]
    BeginScope {
        request_id: String,
        scope_id: String,
        roles: Vec<observed::Role>,
        phase: String,
    },
    #[serde(rename_all = "camelCase")]
    EndScope {
        request_id: String,
        scope_id: String,
    },
    #[serde(rename_all = "camelCase")]
    ArmReadBarrier {
        request_id: String,
        scope_id: String,
        generation: u64,
        barrier_id: String,
        hash: String,
        flow: String,
    },
    #[serde(rename_all = "camelCase")]
    ReleaseReadBarrier {
        request_id: String,
        scope_id: String,
        generation: u64,
        barrier_id: String,
        hash: String,
        flow: String,
    },
    #[serde(rename_all = "camelCase")]
    CancelReadBarrier {
        request_id: String,
        scope_id: String,
        generation: u64,
        barrier_id: String,
        hash: String,
        flow: String,
    },
    #[serde(rename_all = "camelCase")]
    Shutdown { request_id: String },
}
impl Command {
    fn id(&self) -> &str {
        match self {
            Self::Start { request_id, .. }
            | Self::Register { request_id, .. }
            | Self::BeginScope { request_id, .. }
            | Self::EndScope { request_id, .. }
            | Self::ArmReadBarrier { request_id, .. }
            | Self::ReleaseReadBarrier { request_id, .. }
            | Self::CancelReadBarrier { request_id, .. }
            | Self::Shutdown { request_id } => request_id,
        }
    }
}
fn read_command(input: &mut impl BufRead) -> Result<Option<Command>, String> {
    read_command_bounded(input, MAX_COMMAND_BYTES)
}
fn read_command_bounded(
    input: &mut impl BufRead,
    max_bytes: usize,
) -> Result<Option<Command>, String> {
    let mut bytes = Vec::new();
    loop {
        let chunk = input.fill_buf().map_err(|_| "stdin-failed")?;
        if chunk.is_empty() {
            if bytes.is_empty() {
                return Ok(None);
            }
            return Err("unterminated-command".into());
        }
        let count = chunk
            .iter()
            .position(|v| *v == b'\n')
            .map_or(chunk.len(), |n| n + 1);
        if bytes.len().checked_add(count).is_none_or(|n| n > max_bytes) {
            return Err("command-too-large".into());
        }
        let done = chunk[count - 1] == b'\n';
        bytes.extend_from_slice(&chunk[..count]);
        input.consume(count);
        if done {
            break;
        }
    }
    let command: Command = serde_json::from_slice(&bytes).map_err(|_| "invalid-command")?;
    if command.id().is_empty() || command.id().len() > 128 {
        return Err("invalid-request-id".into());
    }
    Ok(Some(command))
}
fn reply(value: Value) {
    let mut output = std::io::stdout().lock();
    writeln!(output, "\n{PREFIX}{value}").unwrap();
    output.flush().unwrap();
}
fn binary_hash() -> String {
    let mut file = std::fs::File::open(std::env::current_exe().unwrap()).unwrap();
    let mut hash = Sha256::new();
    let mut bytes = [0; 64 * 1024];
    loop {
        let count = file.read(&mut bytes).unwrap();
        if count == 0 {
            break;
        }
        hash.update(&bytes[..count]);
    }
    hex::encode(hash.finalize())
}

#[test]
#[ignore]
fn serve() {
    observed::install();
    observed::read_barrier::install_hook(Some(Arc::new(|event| {
        reply(serde_json::to_value(event).unwrap())
    })));
    let mut input = std::io::stdin().lock();
    let first = read_command(&mut input)
        .expect("invalid initial private command")
        .expect("missing Start");
    let Command::Start {
        request_id,
        run_id,
        expected_source_fingerprint,
    } = first
    else {
        panic!("Start required")
    };
    assert!(
        !run_id.is_empty() && run_id.len() <= 128,
        "invalid run identity"
    );
    let (fingerprint, closure) = source_identity();
    assert_eq!(
        fingerprint, expected_source_fingerprint,
        "compiled source identity differs"
    );
    let binary = binary_hash();
    let root = tempfile::Builder::new()
        .prefix("risunest-source-observer-")
        .tempdir()
        .unwrap();
    let root_path = root.path().to_owned();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let mut server = runtime.block_on(Server::start(root.path())).unwrap();
    reply(
        json!({"type":"ready","requestId":request_id,"runId":run_id,"endpoint":server.endpoint,"rootId":root_path,"sourceFingerprint":fingerprint,"binarySha256":binary,"observerSchema":1,"maxCommandBytes":MAX_COMMAND_BYTES,"readBarrierHoldLimitMillis":observed::read_barrier::HOLD_LIMIT.as_millis(),"sourceClosure":closure,"sourceComposition":"same-source-linked-object-delta-stream-transfer","structuralVerification":"normal-linked-recipe-field-move-and-frame-encode-parity"}),
    );
    loop {
        let command = match read_command(&mut input)
            .and_then(|command| command.ok_or_else(|| "stdin-eof-before-shutdown".into()))
        {
            Ok(command) => command,
            Err(code) => {
                observed::note_violation("private-command-error");
                let shutdown = runtime.block_on(server.stop());
                runtime.shutdown_timeout(DRAIN_LIMIT);
                let cleanup = root.close();
                reply(
                    json!({"type":"error","error":code,"complete":false,"rootRemoved":!root_path.exists(),"cleanupComplete":shutdown.is_ok() && cleanup.is_ok()}),
                );
                panic!("invalid private command");
            }
        };
        let id = command.id().to_owned();
        let result:Result<Value,String>=match command {
            Command::Start {..}=>Err("already-started".into()),
            Command::Register {name,registration_request_id,..}=>{
                if observed::snapshot(false).is_some_and(|scope|server.held.is_none() || !scope.complete) {Err("registration-during-observation".into())}
                else {server.store.issue_named_registration(&name,&registration_request_id,Some(&server.endpoint)).map(|uri|json!({"type":"registration","uri":uri})).map_err(|e|e.code.into())}
            }
            Command::BeginScope {scope_id,phase,roles,..}=>runtime.block_on(server.begin(&root_path,scope_id.clone(),phase,roles)).map(|generation|json!({"type":"scopeStarted","scopeId":scope_id,"generation":generation,"completeReset":true})),
            Command::EndScope {scope_id,..}=>runtime.block_on(server.end(&scope_id)).map(|snapshot|json!({"type":"scopeEnded","observation":snapshot,"sourceFingerprint":fingerprint,"binarySha256":binary})),
            Command::ArmReadBarrier {scope_id,generation,barrier_id,hash,flow,..}=> observed::read_barrier::arm(observed::read_barrier::Intent{scope_id,generation,barrier_id,hash,flow}).map(|barrier|json!({"type":"readBarrierArmed","barrier":barrier})).map_err(str::to_owned),
            Command::ReleaseReadBarrier {scope_id,generation,barrier_id,hash,flow,..}=> observed::read_barrier::release(&observed::read_barrier::Intent{scope_id,generation,barrier_id,hash,flow}).map(|barrier|json!({"type":"readBarrierReleased","barrier":barrier})).map_err(str::to_owned),
            Command::CancelReadBarrier {scope_id,generation,barrier_id,hash,flow,..}=> observed::read_barrier::cancel(&observed::read_barrier::Intent{scope_id,generation,barrier_id,hash,flow}).map(|barrier|json!({"type":"readBarrierCancelled","barrier":barrier})).map_err(str::to_owned),
            Command::Shutdown {..}=>{
                let result=runtime.block_on(server.stop());
                runtime.shutdown_timeout(DRAIN_LIMIT);
                let cleanup=root.close().map_err(|_|"synthetic-cleanup-failed".to_owned());
                let success=result.and_then(|observation|cleanup.map(|()|observation));
                reply(match success {Ok(observation)=>json!({"type":"stopped","requestId":id,"rootRemoved":!root_path.exists(),"complete":observation.as_ref().is_none_or(|scope|scope.complete),"observation":observation}),Err(code)=>json!({"type":"error","requestId":id,"error":code})});
                return;
            }
        };
        let mut value = result.unwrap_or_else(|code| json!({"type":"error","error":code}));
        value["requestId"] = id.into();
        reply(value);
    }
}

include!("source_observer/source_identity.rs");

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{source_observer::tests::Reset, store::Device};
    use risunest_sync_wire::hash;
    fn role(digest: &str, purpose: &str) -> observed::Role {
        observed::Role {
            hash: digest.into(),
            purposes: [purpose.to_owned()].into_iter().collect(),
        }
    }
    fn auth(
        request: reqwest::RequestBuilder,
        credential: &crate::store::DeviceCredential,
    ) -> reqwest::RequestBuilder {
        request
            .bearer_auth(&credential.token)
            .header("x-risu-library", &credential.library_id)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "runs in the observer child process"]
    async fn actual_tcp_no_content_and_full_content_length_bodies_complete() {
        let _reset = Reset::new();
        let root = tempfile::tempdir().unwrap();
        let mut server = Server::start(root.path()).await.unwrap();
        let credential = server.store.add_device().unwrap();
        let device = Device {
            id: credential.device_id.clone(),
        };
        let asset = vec![41; 98_304];
        let digest = hash(&asset);
        server.store.put_object(&device, &digest, &asset).unwrap();
        server
            .begin(
                root.path(),
                "http-body-completion".into(),
                "source".into(),
                vec![role(&digest, "Asset")],
            )
            .await
            .unwrap();
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        for request in [
            client
                .post(format!("{}/ack", server.endpoint))
                .body(br#"{"seq":"0"}"#.to_vec()),
            client
                .post(format!("{}/objects/pins", server.endpoint))
                .body(b"[]".to_vec()),
            client.delete(format!("{}/state/pins/synthetic-pin", server.endpoint)),
        ] {
            let response = auth(request, &credential).send().await.unwrap();
            assert_eq!(response.status(), StatusCode::NO_CONTENT);
            assert!(response.bytes().await.unwrap().is_empty());
        }
        let response = auth(
            client.get(format!("{}/objects/{digest}", server.endpoint)),
            &credential,
        )
        .send()
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.content_length(), Some(asset.len() as u64));
        assert_eq!(response.bytes().await.unwrap().as_ref(), asset);
        let response = auth(
            client
                .get(format!("{}/objects/{digest}", server.endpoint))
                .header("if-none-match", format!("\"{digest}\"")),
            &credential,
        )
        .send()
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
        assert!(response.bytes().await.unwrap().is_empty());
        let response = auth(
            client.head(format!("{}/objects/{digest}", server.endpoint)),
            &credential,
        )
        .send()
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()["content-length"],
            asset.len().to_string()
        );
        assert!(response.bytes().await.unwrap().is_empty());
        let ended = server.end("http-body-completion").await.unwrap();
        assert_eq!(ended.total.opens, 3);
        assert_eq!(ended.total.read_bytes, asset.len() as u64);
        assert!(ended.complete, "{:?}", ended.violations);
        assert!(server.stop().await.unwrap().unwrap().complete);
    }

    #[tokio::test]
    #[ignore = "runs in the observer child process"]
    async fn exact_length_completion_without_eof_poll_preserves_prefix_drop_failure() {
        use futures_util::StreamExt;
        let _reset = Reset::new();
        let root = tempfile::tempdir().unwrap();
        observed::begin(root.path(), "body-drop".into(), "source".into(), vec![]).unwrap();
        let gate = Gate::new();
        let mut complete = ResponseStream {
            stream: Body::from("complete").into_data_stream(),
            _permit: gate.lock.clone().read_owned().await,
            _request: observed::request(),
            remaining: Some(8),
            finished: false,
        };
        assert_eq!(
            complete.next().await.unwrap().unwrap().as_ref(),
            b"complete"
        );
        assert!(!complete.finished);
        drop(complete);
        assert!(observed::snapshot(false).unwrap().complete);

        let body = Body::from_stream(futures_util::stream::iter([
            Ok::<_, axum::Error>(Bytes::from_static(b"prefix")),
            Ok(Bytes::from_static(b"tail")),
        ]));
        let mut prefix = ResponseStream {
            stream: body.into_data_stream(),
            _permit: gate.lock.clone().read_owned().await,
            _request: observed::request(),
            remaining: Some(10),
            finished: false,
        };
        assert_eq!(prefix.next().await.unwrap().unwrap().as_ref(), b"prefix");
        assert_eq!(prefix.remaining, Some(4));
        drop(prefix);
        let ended = observed::snapshot(true).unwrap();
        assert!(!ended.complete);
        assert_eq!(ended.pending_requests, 0);
        assert_eq!(ended.violations.get("response-body-abandoned"), Some(&1));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "runs in the observer child process"]
    async fn actual_tcp_raw_media_ranges_zero_and_exact_missing_subset() {
        use risunest_sync_connect::media::{MediaObject, MediaRequest, MediaSigner};
        let _reset = Reset::new();
        let root = tempfile::tempdir().unwrap();
        let mut server = Server::start(root.path()).await.unwrap();
        let credential = server.store.add_device().unwrap();
        let device = Device {
            id: credential.device_id.clone(),
        };
        let inline = b"synthetic-inline-control";
        let a = vec![13; 65_537];
        let b = vec![19; 65_539];
        for body in [inline.as_slice(), &a, &b] {
            server.store.put_object(&device, &hash(body), body).unwrap();
        }
        let object = MediaObject {
            hash: hash(&a),
            size: (a.len() as u64).into(),
            mime: "image/custom".into(),
        };
        let refresh = MediaSigner::new(&[3; 32])
            .unwrap()
            .refresh_url(&server.endpoint, &object)
            .unwrap();
        let grant = server
            .store
            .media_access(
                &device,
                &server.store.head().unwrap().epoch,
                &[MediaRequest {
                    object,
                    refresh_url: refresh,
                }],
            )
            .unwrap()
            .remove(0);
        let roles = vec![
            role(&hash(inline), "Control"),
            role(&hash(&a), "Asset"),
            role(&hash(&b), "Asset"),
        ];
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        server
            .begin(
                root.path(),
                "positive".into(),
                "positive-control".into(),
                roles.clone(),
            )
            .await
            .unwrap();
        let response = auth(
            client.get(format!("{}/objects/{}", server.endpoint, hash(inline))),
            &credential,
        )
        .header("range", "bytes=2-5")
        .send()
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(response.bytes().await.unwrap().as_ref(), &inline[2..6]);
        let response = client
            .get(format!("{}/media/{}", server.endpoint, grant.token))
            .header("range", "bytes=3-9")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(response.bytes().await.unwrap().len(), 7);
        let positive = server.end("positive").await.unwrap();
        assert!(positive.complete, "{:?}", positive.violations);
        assert_eq!(
            positive.placements["inline"].read_bytes,
            inline.len() as u64
        );
        assert_eq!(positive.placements["file"].read_bytes, 7);
        assert_eq!(positive.total.opens, 2);
        assert_eq!(positive.total.hash_bytes, inline.len() as u64);
        assert!(positive.objects.iter().any(|row| row.hash == hash(&a)
            && row
                .selected_ranges
                .iter()
                .any(|range| range.start == 3 && range.bytes == 7)));
        assert!(positive.objects.iter().any(|row| row.hash == hash(inline)
            && row
                .selected_ranges
                .iter()
                .any(|range| range.start == 2 && range.bytes == 4)));
        server
            .begin(
                root.path(),
                "all-present".into(),
                "restore".into(),
                roles.clone(),
            )
            .await
            .unwrap();
        let response=auth(client.post(format!("{}/objects/missing",server.endpoint)),&credential).json(&json!([{"hash":hash(&a),"size":a.len().to_string()},{"hash":hash(&b),"size":b.len().to_string()}])).send().await.unwrap();
        assert!(response.status().is_success());
        let _: Value = response.json().await.unwrap();
        let zero = server.end("all-present").await.unwrap();
        assert!(zero.complete, "{:?}", zero.violations);
        assert_eq!(zero.total, observed::Work::default());
        server
            .begin(
                root.path(),
                "missing-subset".into(),
                "hydrate".into(),
                roles,
            )
            .await
            .unwrap();
        let response = auth(
            client.get(format!("{}/objects/{}", server.endpoint, hash(&b))),
            &credential,
        )
        .send()
        .await
        .unwrap();
        assert_eq!(response.bytes().await.unwrap().as_ref(), b);
        let subset = server.end("missing-subset").await.unwrap();
        assert!(subset.complete, "{:?}", subset.violations);
        assert_eq!(subset.objects.len(), 1);
        assert_eq!(subset.objects[0].hash, hash(&b));
        assert_eq!(subset.total.opens, 1);
        assert_eq!(subset.total.read_bytes, b.len() as u64);
        assert_eq!(subset.total.hash_calls, 0);
        assert!(server.stop().await.unwrap().unwrap().complete);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "runs in the observer child process"]
    async fn actual_tcp_first_read_barrier_keeps_foreground_body_requests_running() {
        let _reset = Reset::new();
        let root = tempfile::tempdir().unwrap();
        let mut server = Server::start(root.path()).await.unwrap();
        let credential = server.store.add_device().unwrap();
        let device = Device {
            id: credential.device_id.clone(),
        };
        let asset = vec![37; 65_537];
        let control = b"synthetic other unit body";
        let asset_hash = hash(&asset);
        let control_hash = hash(control);
        server
            .store
            .put_object(&device, &asset_hash, &asset)
            .unwrap();
        server
            .store
            .put_object(&device, &control_hash, control)
            .unwrap();
        let generation = server
            .begin(
                root.path(),
                "tcp-held".into(),
                "asset-transfer".into(),
                vec![role(&asset_hash, "Asset"), role(&control_hash, "Control")],
            )
            .await
            .unwrap();
        let intent = observed::read_barrier::Intent {
            scope_id: "tcp-held".into(),
            generation,
            barrier_id: "tcp-first-read".into(),
            hash: asset_hash.clone(),
            flow: "source".into(),
        };
        let (send, mut receive) = tokio::sync::mpsc::unbounded_channel();
        observed::read_barrier::install_hook(Some(Arc::new(move |event| {
            send.send(serde_json::to_value(event).unwrap()).unwrap()
        })));
        observed::read_barrier::arm(intent.clone()).unwrap();
        let client = reqwest::Client::new();
        let request = auth(
            client.get(format!("{}/objects/{}", server.endpoint, asset_hash)),
            &credential,
        );
        let body =
            tokio::spawn(async move { request.send().await.unwrap().bytes().await.unwrap() });
        let event = tokio::time::timeout(DRAIN_LIMIT, receive.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(event["readKind"], "asyncRead");
        assert_eq!(event["placement"], "file");
        assert_eq!(event["hash"], asset_hash);
        assert_eq!(event["offset"], 0);
        let held = observed::snapshot(false).unwrap();
        assert!(!held.complete);
        assert_eq!(held.total.read_bytes, 0);
        assert!(held.pending_requests > 0);
        assert_eq!(held.read_barrier.unwrap().waiters, 1);
        let foreground = tokio::time::timeout(
            DRAIN_LIMIT,
            auth(
                client.get(format!("{}/objects/{}", server.endpoint, control_hash)),
                &credential,
            )
            .send(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(foreground.status(), StatusCode::OK);
        assert_eq!(foreground.bytes().await.unwrap().as_ref(), control);
        assert!(!body.is_finished());
        assert_eq!(
            observed::snapshot(false).unwrap().purposes["Asset"].read_bytes,
            0
        );
        observed::read_barrier::release(&intent).unwrap();
        assert_eq!(
            tokio::time::timeout(DRAIN_LIMIT, body)
                .await
                .unwrap()
                .unwrap()
                .as_ref(),
            asset
        );
        let done = server.end("tcp-held").await.unwrap();
        assert!(done.complete, "{:?}", done.violations);
        assert_eq!(done.purposes["Asset"].read_bytes, asset.len() as u64);
        assert_eq!(done.purposes["Control"].read_bytes, control.len() as u64);
        assert_eq!(done.read_barrier.unwrap().status, "released");
        assert!(server.stop().await.unwrap().unwrap().complete);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "runs in the observer child process"]
    async fn actual_tcp_dangling_read_barrier_end_cancels_and_cannot_reset() {
        let _reset = Reset::new();
        let root = tempfile::tempdir().unwrap();
        let mut server = Server::start(root.path()).await.unwrap();
        let credential = server.store.add_device().unwrap();
        let device = Device {
            id: credential.device_id.clone(),
        };
        let asset = vec![43; 65_537];
        let asset_hash = hash(&asset);
        server
            .store
            .put_object(&device, &asset_hash, &asset)
            .unwrap();
        let generation = server
            .begin(
                root.path(),
                "tcp-cancel".into(),
                "asset-transfer".into(),
                vec![role(&asset_hash, "Asset")],
            )
            .await
            .unwrap();
        let intent = observed::read_barrier::Intent {
            scope_id: "tcp-cancel".into(),
            generation,
            barrier_id: "tcp-cancel-first".into(),
            hash: asset_hash.clone(),
            flow: "source".into(),
        };
        let (send, mut receive) = tokio::sync::mpsc::unbounded_channel();
        observed::read_barrier::install_hook(Some(Arc::new(move |event| {
            send.send(event).unwrap()
        })));
        observed::read_barrier::arm(intent).unwrap();
        let request = auth(
            reqwest::Client::new().get(format!("{}/objects/{}", server.endpoint, asset_hash)),
            &credential,
        );
        let body = tokio::spawn(async move {
            match request.send().await {
                Ok(response) => response.bytes().await.map(|_| ()),
                Err(error) => Err(error),
            }
        });
        tokio::time::timeout(DRAIN_LIMIT, receive.recv())
            .await
            .unwrap()
            .unwrap();
        let ended = server.end("tcp-cancel").await.unwrap();
        assert!(!ended.complete);
        assert_eq!(ended.read_barrier.unwrap().status, "cancelled");
        assert!(tokio::time::timeout(DRAIN_LIMIT, body)
            .await
            .unwrap()
            .unwrap()
            .is_err());
        assert!(server
            .begin(
                root.path(),
                "reset-after-cancel".into(),
                "source".into(),
                vec![]
            )
            .await
            .is_err());
        assert!(!server.stop().await.unwrap().unwrap().complete);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "runs in the observer child process"]
    async fn all_router_barrier_tracks_held_media_and_rejects_late_admission() {
        let _reset = Reset::new();
        let root = tempfile::tempdir().unwrap();
        let mut server = Server::start(root.path()).await.unwrap();
        server
            .begin(root.path(), "held-media".into(), "error".into(), vec![])
            .await
            .unwrap();
        let request = observed::request();
        let permit = server.gate.lock.clone().read_owned().await;
        let body = Body::from("real response prefix");
        let stream = ResponseStream {
            remaining: None,
            finished: false,
            stream: body.into_data_stream(),
            _permit: permit,
            _request: request,
        };
        assert_eq!(observed::snapshot(false).unwrap().pending_requests, 1);
        let held = tokio::time::timeout(
            Duration::from_millis(25),
            server.gate.lock.clone().write_owned(),
        )
        .await;
        assert!(held.is_err());
        drop(stream);
        assert!(observed::snapshot(false)
            .unwrap()
            .violations
            .contains_key("response-body-abandoned"));
        let ended = server.end("held-media").await.unwrap();
        assert!(!ended.complete);
        assert_eq!(ended.pending_requests, 0);
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        assert_eq!(
            client
                .get(format!("{}/head", server.endpoint))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert!(observed::snapshot(false)
            .unwrap()
            .violations
            .contains_key("request-after-barrier"));
        assert!(server
            .begin(root.path(), "reset".into(), "test".into(), vec![])
            .await
            .is_err());
        assert!(!server.stop().await.unwrap().unwrap().complete);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "runs in the observer child process"]
    async fn queued_body_work_cannot_be_hidden_by_zero_active_workers() {
        let _reset = Reset::new();
        let root = tempfile::tempdir().unwrap();
        let mut server = Server::start(root.path()).await.unwrap();
        let credential = server.store.add_device().unwrap();
        let device = Device {
            id: credential.device_id,
        };
        server
            .begin(
                root.path(),
                "pending-durable".into(),
                "error".into(),
                vec![],
            )
            .await
            .unwrap();
        let upload = server
            .store
            .begin_upload(
                &device,
                &crate::store::UploadManifest {
                    hash: hash(b"synthetic pending upload"),
                    size: 24.into(),
                },
            )
            .unwrap();
        let measured = server.end("pending-durable").await.unwrap();
        assert!(!measured.complete);
        assert_eq!(measured.pending_workers, 0);
        assert_eq!(measured.pending_requests, 0);
        assert_eq!(measured.pending_body_work.upload_sessions, 1);
        assert!(measured
            .violations
            .contains_key("pending-durable-body-work"));
        server.store.cancel_upload(&device, &upload).unwrap();
        assert!(server
            .begin(root.path(), "reset".into(), "test".into(), vec![])
            .await
            .is_err());
        assert!(!server.stop().await.unwrap().unwrap().complete);
    }

    #[test]
    fn private_command_bounds_and_compiled_identity() {
        let valid=br#"{"op":"start","requestId":"r","runId":"synthetic","expectedSourceFingerprint":"abc"}
"#;
        assert!(
            matches!(read_command(&mut &valid[..]).unwrap(),Some(Command::Start {request_id,..}) if request_id=="r")
        );
        assert!(read_command(
            &mut &b"{\"op\":\"shutdown\",\"requestId\":\"r\",\"proof\":true}\n"[..]
        )
        .is_err());
        assert!(read_command(&mut &b"{\"op\":\"shutdown\",\"requestId\":\"r\"}"[..]).is_err());
        assert!(matches!(
            read_command_bounded(&mut &valid[..], valid.len()).unwrap(),
            Some(Command::Start { .. })
        ));
        assert_eq!(
            read_command_bounded(&mut &valid[..], valid.len() - 1)
                .err()
                .unwrap(),
            "command-too-large"
        );
        let (fingerprint, entries) = source_identity();
        assert_eq!(fingerprint.len(), 64);
        let identity = entries
            .iter()
            .find(|entry| entry["path"] == "server/sync/src/identity.rs")
            .expect("the UUID implementation must be part of the compiled source identity");
        assert_eq!(identity["role"], "server");
        assert_eq!(
            identity["sha256"],
            hex::encode(Sha256::digest(include_bytes!("identity.rs")))
        );
        for path in [
            "crates/small-object-store/src/lib.rs",
            "crates/sync-wire/src/delta.rs",
            "crates/sync-wire/src/stream_delta.rs",
        ] {
            let matching = entries
                .iter()
                .filter(|entry| entry["path"] == path)
                .collect::<Vec<_>>();
            assert_eq!(matching.len(), 2);
            assert_eq!(matching[0]["sha256"], matching[1]["sha256"]);
            assert_ne!(matching[0]["role"], matching[1]["role"]);
        }
        assert!(entries
            .iter()
            .any(|entry| entry["path"] == "server/sync/src/source_observer_harness.rs"));
    }

    #[test]
    fn separate_libtest_process_protocol_fingerprint_and_cleanup() {
        separate_process_protocol(0);
    }
    #[test]
    fn separate_libtest_invalid_command_is_terminal_and_cleans_up() {
        separate_process_protocol(1);
    }
    #[test]
    fn separate_libtest_eof_unblocks_actual_held_read_and_cleans_up() {
        separate_process_protocol(2);
    }
    #[test]
    fn separate_libtest_read_barrier_release_protocol_and_cleanup() {
        separate_process_protocol(3);
    }
    fn separate_process_protocol(case: u8) {
        use std::{
            io::BufReader,
            process::{Command as Process, Stdio},
            sync::mpsc,
        };
        struct Child(std::process::Child);
        impl Drop for Child {
            fn drop(&mut self) {
                if self.0.try_wait().unwrap().is_none() {
                    self.0.kill().unwrap();
                    self.0.wait().unwrap();
                }
            }
        }
        let (fingerprint, _) = source_identity();
        let expected_binary = binary_hash();
        let mut child = Child(
            Process::new(std::env::current_exe().unwrap())
                .args([
                    "--ignored",
                    "--exact",
                    "source_observer_harness::serve",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        );
        let mut input = child.0.stdin.take().unwrap();
        let output = child.0.stdout.take().unwrap();
        let (send, receive) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(output).lines() {
                let line = line.unwrap();
                if let Some(line) = line.strip_prefix(PREFIX) {
                    send.send(serde_json::from_str::<Value>(line).unwrap())
                        .unwrap();
                }
            }
        });
        let send_command = |input: &mut std::process::ChildStdin, value: Value| {
            writeln!(input, "{value}").unwrap();
            input.flush().unwrap();
        };
        send_command(
            &mut input,
            json!({"op":"start","requestId":"start","runId":"synthetic-protocol","expectedSourceFingerprint":fingerprint}),
        );
        let ready = receive.recv_timeout(DRAIN_LIMIT).unwrap();
        assert_eq!(ready["type"], "ready");
        assert_eq!(ready["sourceFingerprint"], fingerprint);
        assert_eq!(ready["binarySha256"], expected_binary);
        assert_eq!(ready["maxCommandBytes"], MAX_COMMAND_BYTES);
        let root = std::path::PathBuf::from(ready["rootId"].as_str().unwrap());
        assert!(root.exists());
        send_command(
            &mut input,
            json!({"op":"register","requestId":"registration","name":"Synthetic protocol device","registrationRequestId":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"}),
        );
        let registration = receive.recv_timeout(DRAIN_LIMIT).unwrap();
        assert_eq!(registration["type"], "registration");
        let parsed =
            risunest_sync_connect::Registration::parse_uri(registration["uri"].as_str().unwrap())
                .unwrap();
        assert_eq!(parsed.endpoint, ready["endpoint"]);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let asset = vec![71; 65_537];
        let asset_hash = hash(&asset);
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        if case >= 2 {
            let frames =
                risunest_sync_wire::transfer::encode(&[risunest_sync_wire::transfer::Frame::Full(
                    asset.clone(),
                )])
                .unwrap();
            let uploaded = runtime
                .block_on(
                    client
                        .post(format!("{}/uploads/frames", parsed.endpoint))
                        .bearer_auth(&parsed.token)
                        .header("x-risu-library", &parsed.library_id)
                        .body(frames)
                        .send(),
                )
                .unwrap();
            assert!(uploaded.status().is_success());
            runtime.block_on(uploaded.bytes()).unwrap();
        }
        let roles = if case >= 2 {
            json!([{"hash":asset_hash,"purposes":["Asset"]}])
        } else {
            json!([])
        };
        send_command(
            &mut input,
            json!({"op":"beginScope","requestId":"begin","scopeId":"zero","phase":"protocol-control","roles":roles}),
        );
        let scope = receive.recv_timeout(DRAIN_LIMIT).unwrap();
        assert_eq!(scope["type"], "scopeStarted");
        if case >= 2 {
            let intent = json!({"scopeId":"zero","generation":scope["generation"],"barrierId":"process-first-read","hash":asset_hash,"flow":"source"});
            let mut arm = intent.clone();
            arm["op"] = "armReadBarrier".into();
            arm["requestId"] = "arm".into();
            send_command(&mut input, arm);
            let armed = receive.recv_timeout(DRAIN_LIMIT).unwrap();
            assert_eq!(armed["type"], "readBarrierArmed");
            let request = client
                .get(format!("{}/objects/{}", parsed.endpoint, asset_hash))
                .bearer_auth(&parsed.token)
                .header("x-risu-library", &parsed.library_id);
            let download = runtime.spawn(async move {
                match request.send().await {
                    Ok(response) => response.bytes().await,
                    Err(error) => Err(error),
                }
            });
            let reached = receive.recv_timeout(DRAIN_LIMIT).unwrap();
            assert_eq!(reached["type"], "readBarrierReached");
            assert_eq!(reached["hash"], asset_hash);
            assert_eq!(reached["readKind"], "asyncRead");
            if case == 2 {
                drop(input);
                let error = receive.recv_timeout(DRAIN_LIMIT).unwrap();
                assert_eq!(error["type"], "error");
                assert_eq!(error["error"], "stdin-eof-before-shutdown");
                assert_eq!(error["complete"], false);
                assert_eq!(error["cleanupComplete"], true);
                assert_eq!(error["rootRemoved"], true);
                assert!(!root.exists());
                assert!(runtime.block_on(download).unwrap().is_err());
                assert!(!child.0.wait().unwrap().success());
                reader.join().unwrap();
                runtime.shutdown_timeout(DRAIN_LIMIT);
                return;
            }
            let mut release = intent;
            release["op"] = "releaseReadBarrier".into();
            release["requestId"] = "release".into();
            send_command(&mut input, release);
            assert_eq!(
                receive.recv_timeout(DRAIN_LIMIT).unwrap()["type"],
                "readBarrierReleased"
            );
            assert_eq!(runtime.block_on(download).unwrap().unwrap().as_ref(), asset);
        }
        if case == 1 {
            send_command(
                &mut input,
                json!({"op":"shutdown","requestId":"bad","omittedProof":true}),
            );
            let error = receive.recv_timeout(DRAIN_LIMIT).unwrap();
            assert_eq!(error["type"], "error");
            assert_eq!(error["error"], "invalid-command");
            assert_eq!(error["complete"], false);
            assert_eq!(error["cleanupComplete"], true);
            assert_eq!(error["rootRemoved"], true);
            assert!(!root.exists());
            drop(input);
            assert!(!child.0.wait().unwrap().success());
            reader.join().unwrap();
            runtime.shutdown_timeout(DRAIN_LIMIT);
            return;
        }
        send_command(
            &mut input,
            json!({"op":"endScope","requestId":"end","scopeId":"zero"}),
        );
        let end = receive.recv_timeout(DRAIN_LIMIT).unwrap();
        assert_eq!(end["type"], "scopeEnded");
        assert_eq!(end["observation"]["complete"], true);
        assert_eq!(end["binarySha256"], expected_binary);
        send_command(&mut input, json!({"op":"shutdown","requestId":"shutdown"}));
        let stopped = receive.recv_timeout(DRAIN_LIMIT).unwrap();
        assert_eq!(stopped["type"], "stopped");
        assert_eq!(stopped["rootRemoved"], true);
        assert_eq!(stopped["complete"], true);
        assert!(!root.exists());
        drop(input);
        assert!(child.0.wait().unwrap().success());
        reader.join().unwrap();
        runtime.shutdown_timeout(DRAIN_LIMIT);
    }
}
