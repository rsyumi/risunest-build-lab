use super::{
    commands::{self, NormalizationInput, Reservation, RouteDispatch, RouteOutput, RouteStatus},
    operation::{Phase, Ticket},
};
use crate::persistent_store::{
    commands::{with_store, with_store_mut},
    generation::CancelReason,
    PersistentStore, PersistentStoreState,
};
use loopback::serve;
use risunest_sync_wire::unit::UnitKey;
use serde_json::{json, Value};
use std::{collections::BTreeSet, sync::atomic::Ordering, time::Duration};
use tauri::{test::MockRuntime, AppHandle, Manager};

const USER_AGENT: &str = "Mozilla/5.0 (Synthetic) RisuNest-command-test";

mod loopback {
    use std::{sync::{Arc, Mutex}, time::Duration};
    use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::{TcpListener, TcpStream}, sync::watch, task::JoinHandle};

    struct Shared {
        status: &'static str,
        body: String,
        requests: watch::Sender<usize>,
        dropped: watch::Sender<usize>,
        released: watch::Sender<bool>,
        user_agents: Mutex<Vec<String>>,
    }

    pub struct Server { pub origin: String, shared: Arc<Shared>, task: JoinHandle<()> }

    impl Drop for Server { fn drop(&mut self) { self.task.abort(); } }

    // A guard against a hung owner, not a timing assertion.
    async fn reached(counter: &watch::Sender<usize>, count: usize) {
        let mut receiver = counter.subscribe();
        tokio::time::timeout(Duration::from_secs(30), receiver.wait_for(|value| *value >= count)).await
            .expect("loopback condition was not reached").unwrap();
    }

    impl Server {
        pub fn requests(&self) -> usize { *self.shared.requests.borrow() }
        pub fn dropped(&self) -> usize { *self.shared.dropped.borrow() }
        pub fn user_agents(&self) -> Vec<String> { self.shared.user_agents.lock().unwrap().clone() }
        pub fn release(&self) { self.shared.released.send_replace(true); }
        pub async fn requested(&self, count: usize) { reached(&self.shared.requests, count).await }
        pub async fn closed_by_client(&self, count: usize) { reached(&self.shared.dropped, count).await }
    }

    // A held reply waits for `release` or for the client to close the connection.
    pub async fn serve(status: &'static str, body: &str, hold: bool) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let shared = Arc::new(Shared { status, body: body.to_owned(), requests: watch::channel(0).0, dropped: watch::channel(0).0,
            released: watch::channel(!hold).0, user_agents: Mutex::new(Vec::new()) });
        let accepted = shared.clone();
        let task = tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await { tokio::spawn(handle(stream, accepted.clone())); }
        });
        Server { origin, shared, task }
    }

    async fn handle(mut stream: TcpStream, shared: Arc<Shared>) {
        let Some(agent) = read_request(&mut stream).await else { return };
        shared.user_agents.lock().unwrap().push(agent);
        shared.requests.send_modify(|value| *value += 1);
        let mut released = shared.released.subscribe();
        let release = async move { while !*released.borrow_and_update() { if released.changed().await.is_err() { break; } } };
        let mut probe = [0; 1];
        tokio::select! {
            _ = release => {}
            _ = stream.read(&mut probe) => { shared.dropped.send_modify(|value| *value += 1); return; }
        }
        let head = format!("HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", shared.status, shared.body.len());
        if stream.write_all(head.as_bytes()).await.is_ok() && stream.write_all(shared.body.as_bytes()).await.is_ok() { let _ = stream.shutdown().await; }
    }

    async fn read_request(stream: &mut TcpStream) -> Option<String> {
        let mut data = Vec::new();
        let mut block = [0; 4096];
        let head = loop {
            if let Some(index) = data.windows(4).position(|window| window == b"\r\n\r\n") { break index + 4; }
            let read = stream.read(&mut block).await.ok()?;
            if read == 0 { return None; }
            data.extend_from_slice(&block[..read]);
        };
        let text = std::str::from_utf8(&data[..head]).ok()?.to_owned();
        let header = |name: &str| text.split("\r\n").filter_map(|line| line.split_once(':'))
            .find(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value.trim().to_owned());
        let length = header("content-length").map_or(Some(0), |value| value.parse::<usize>().ok())?;
        while data.len() < head + length {
            let read = stream.read(&mut block).await.ok()?;
            if read == 0 { return None; }
            data.extend_from_slice(&block[..read]);
        }
        Some(header("user-agent").unwrap_or_default())
    }
}

fn reply(content: Value) -> String {
    json!({ "model": "synthetic-model", "created_at": "2026-10-25T00:00:00Z", "message": { "role": "assistant", "content": content }, "done": true }).to_string()
}

fn fixture() -> Value {
    serde_json::from_str(include_str!("../../../tests/fixtures/native-generation/ollama-text.json")).unwrap()
}

// Fields drop in order: the app and its store close before the directory is removed.
struct Harness { app: tauri::App<MockRuntime>, _directory: tempfile::TempDir }

fn setup(origin: &str, root: Value, messages: Option<Value>) -> Harness {
    let fixture = fixture();
    let mut database = fixture["root"].clone();
    database["ollamaURL"] = json!(origin);
    for (key, value) in root.as_object().unwrap() { database[key] = value.clone(); }
    let mut conversation = fixture["conversation"].clone();
    conversation["message"] = messages.unwrap_or_else(|| fixture["messages"].clone());
    let mut character = fixture["character"].clone();
    character["chats"] = json!([conversation]);
    let directory = tempfile::tempdir().unwrap();
    let mut store = PersistentStore::open(directory.path()).unwrap();
    let stage = store.replace_begin().unwrap();
    store.replace_put_root(&stage.staging_id, &database).unwrap();
    store.replace_put_presets(&stage.staging_id, &[fixture["preset"].clone()]).unwrap();
    store.replace_add_characters(&stage.staging_id, &[character]).unwrap();
    store.replace_commit(&stage.staging_id, Some(0)).unwrap();
    let app = tauri::test::mock_builder().build(tauri::test::mock_context(tauri::test::noop_assets())).unwrap();
    app.manage(PersistentStoreState::with_test_store(store));
    commands::host_init(USER_AGENT).unwrap();
    Harness { app, _directory: directory }
}

// A guard against a hung owner, not a timing assertion.
async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(60), future).await.expect("native generation did not progress")
}

impl Harness {
    fn handle(&self) -> AppHandle<MockRuntime> { self.app.handle().clone() }
    fn state(&self) -> tauri::State<'_, PersistentStoreState> { self.app.state::<PersistentStoreState>() }
    fn revision(&self) -> i64 { with_store(self.state(), |store| store.revision()).unwrap() }
    fn messages(&self) -> Vec<Value> {
        with_store(self.state(), |store| store.read_conversation("character", "conversation", None)).unwrap().unwrap().value["message"].as_array().unwrap().clone()
    }
    fn lease(&self, revision: i64) -> Value {
        json!({ "token": with_store_mut(self.state(), |store| store.acquire_revision(revision)).unwrap().lease, "revision": revision })
    }
    async fn reservation(&self) -> Reservation {
        let (revision, library, authority) = with_store(self.state(), |store| Ok((store.revision()?, store.library_identity()?, store.lww_binding_authority()?))).unwrap();
        // The request crosses IPC as the J1 facade writes it.
        let request = serde_json::from_value(json!({ "characterId": "character", "conversationId": "conversation", "expectedRevision": revision,
            "libraryIdentity": library, "bindingAuthority": authority, "effectivePresetId": "preset-a" })).unwrap();
        commands::reserve(self.handle(), request).await.unwrap()
    }
    // A reservation carrying the WebView's host capture as `captureNativeHost` writes it.
    async fn reservation_with_host(&self, host: Value) -> Reservation {
        let (revision, library, authority) = with_store(self.state(), |store| Ok((store.revision()?, store.library_identity()?, store.lww_binding_authority()?))).unwrap();
        let request = serde_json::from_value(json!({ "characterId": "character", "conversationId": "conversation", "expectedRevision": revision,
            "libraryIdentity": library, "bindingAuthority": authority, "effectivePresetId": "preset-a", "host": host })).unwrap();
        commands::reserve(self.handle(), request).await.unwrap()
    }
    async fn reserve(&self) -> Ticket {
        match self.reservation().await { Reservation::Ticket { ticket } => ticket, other => panic!("not reserved: {other:?}") }
    }
    async fn execute(&self, ticket: &Ticket) -> RouteStatus { commands::execute(self.handle(), ticket.clone()).await.unwrap() }
    async fn status(&self, ticket: &Ticket) -> RouteStatus { commands::status(self.handle(), ticket.clone(), None).await.unwrap() }
    // A change between two reads is reported only at the next change, so later reads wait briefly.
    async fn wait_until(&self, ticket: &Ticket, done: impl Fn(&RouteStatus) -> bool) -> RouteStatus {
        bounded(async {
            let mut wait = None;
            loop {
                let status = commands::status(self.handle(), ticket.clone(), wait).await.unwrap();
                if done(&status) { return status; }
                wait = Some(1_000);
            }
        }).await
    }
    async fn settled(&self, ticket: &Ticket) -> RouteStatus { self.wait_until(ticket, |status| status.phase.terminal()).await }
    async fn issued(&self, ticket: &Ticket) -> RouteStatus {
        self.wait_until(ticket, |status| status.dispatch == RouteDispatch::Issued || status.phase.terminal()).await
    }
    async fn active(&self) -> Vec<Ticket> {
        commands::active(self.handle()).await.unwrap().into_iter().map(|status| status.ticket).collect()
    }
}

fn messages_key() -> String { UnitKey::new(&["messages", "character", "conversation"]).unwrap().as_str().to_owned() }

fn effect_keys() -> Vec<String> {
    vec![r#"["root","statics"]"#.into(), r#"["character","character","lastInteraction"]"#.into(),
        r#"["conversation","character","conversation","lastMemory"]"#.into()]
}

fn affected_keys() -> Vec<String> { let mut keys = effect_keys(); keys.push(messages_key()); keys }

fn assert_effect_receipts(proof: &commands::Proof) {
    assert!(!proof.receipts.effects.is_empty());
    for receipt in &proof.receipts.effects {
        assert_eq!(keys(receipt), BTreeSet::from(["requestId", "intentDigest", "receiptRevision"]));
        assert!(!receipt["requestId"].as_str().unwrap().is_empty());
        assert!(!receipt["intentDigest"].as_str().unwrap().is_empty());
        assert!(receipt["receiptRevision"].as_i64().unwrap() <= proof.revision);
    }
}

fn keys(value: &Value) -> BTreeSet<&str> { value.as_object().unwrap().keys().map(String::as_str).collect() }

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn succ_executes_once_and_reports_the_committed_reply_in_the_frozen_shapes() {
    let server = serve("200 OK", &reply(json!(" Synthetic response. ")), false).await;
    let harness = setup(&server.origin, json!({}), None);
    let ticket = harness.reserve().await;
    let first = harness.execute(&ticket).await;
    assert_eq!((first.character_id.as_str(), first.conversation_id.as_str()), ("character", "conversation"));
    // A second execute reports the same operation and never starts another owner.
    assert_eq!(harness.execute(&ticket).await.ticket, ticket);
    let status = harness.settled(&ticket).await;
    assert_eq!((status.phase.clone(), status.dispatch, status.output, status.cancel_decision, status.failure.clone()),
        (Phase::Completed, RouteDispatch::Issued, RouteOutput::Committed, None, None));
    assert!(!status.diagnostics_available);
    assert_eq!(server.requests(), 1);
    assert_eq!(server.user_agents(), [USER_AGENT]);
    let row = harness.messages().last().cloned().unwrap();
    assert_eq!((row["data"].as_str(), row["chatId"].as_str()), (Some("Synthetic response."), Some(status.message_id.as_str())));

    let value = serde_json::to_value(&status).unwrap();
    assert_eq!(keys(&value), BTreeSet::from(["ticket", "characterId", "conversationId", "phase", "messageId", "checkpoint", "committedRevision",
        "pendingCommit", "preparationReady", "preparationError", "dispatch", "output", "cancelDecision", "failure", "diagnosticsAvailable"]));
    assert!(value["ticket"]["epoch"].is_string() && value["ticket"]["sequence"].is_string() && value["checkpoint"] == "1");
    assert_eq!((&value["phase"], &value["dispatch"], &value["output"]), (&json!("completed"), &json!("issued"), &json!("committed")));

    let proof = commands::proof(harness.handle(), ticket.clone(), None).await.unwrap();
    assert_eq!((proof.at, proof.affected_unit_keys.clone(), proof.output_message_id.clone()), ("completed", affected_keys(), Some(status.message_id.clone())));
    assert!(proof.receipts.output.is_some() && proof.receipts.normalization.is_none());
    assert_effect_receipts(&proof);
    assert_eq!((proof.id_deltas.clone(), proof.mapping_unavailable, proof.failed_with_output), (Some(Vec::new()), false, false));
    assert_eq!(proof.revision, harness.revision());
    let value = serde_json::to_value(&proof).unwrap();
    assert_eq!(keys(&value), BTreeSet::from(["ticket", "revision", "at", "affectedUnitKeys", "outputMessageId", "receipts", "idDeltas",
        "mappingUnavailable", "failedWithOutput"]));
    assert!(value["receipts"]["output"]["sourceSequence"].is_string());
    assert!(harness.active().await.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reservation_refusal_names_the_unsupported_setting() {
    let harness = setup("http://127.0.0.1:9", json!({ "useStreaming": true }), None);
    let reservation = harness.reservation().await;
    assert_eq!(serde_json::to_value(&reservation).unwrap(), json!({ "unsupported": "useStreaming" }));
}

// The host capture the reservation carries reaches its admission and, through the journal, the preparation: a persona
// prompt with local time and Unicode 17 case mapping reserves only with one, and the send then completes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_reservation_host_capture_reaches_the_preparation() {
    let server = serve("200 OK", &reply(json!("Synthetic response.")), false).await;
    let personas = json!([{ "id": "persona-a", "name": "Synthetic user", "personaPrompt": "Today is {{date}}. {{upper::\u{a7d3}}}", "note": "",
        "icon": "" }]);
    let harness = setup(&server.origin, json!({ "personas": personas }), None);
    assert_eq!(serde_json::to_value(&harness.reservation().await).unwrap(), json!({ "unsupported": "cbs.local-time" }));
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64;
    let host = json!({ "tailoredCase": false, "caseUnicode": "unicode17", "timeZone": { "transitions": [[now - 86_400_000, 0]], "end": now + 86_400_000 },
        "regexUnicode": "before17", "regexExcluded": false });
    let ticket = match harness.reservation_with_host(host).await { Reservation::Ticket { ticket } => ticket, other => panic!("not reserved: {other:?}") };
    harness.execute(&ticket).await;
    let status = harness.settled(&ticket).await;
    assert_eq!((status.phase.clone(), status.failure.clone()), (Phase::Completed, None));
    assert_eq!(server.requests(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_ticket_native_no_longer_knows_is_reported_by_its_own_error_code() {
    let harness = setup("http://127.0.0.1:9", json!({}), None);
    let ticket = harness.reserve().await;
    let unknown: Ticket = serde_json::from_value(json!({ "epoch": ticket.epoch, "sequence": "999" })).unwrap();
    let expired: Ticket = serde_json::from_value(json!({ "epoch": "synthetic-previous-epoch", "sequence": "1" })).unwrap();
    for (ticket, code) in [(unknown, "unknown-ticket"), (expired, "expired-ticket")] {
        let error = commands::status(harness.handle(), ticket.clone(), None).await.unwrap_err();
        assert_eq!(serde_json::to_value(&error).unwrap(), json!({ "code": "validation", "message": format!("native-generation:{code}") }));
        assert!(commands::execute(harness.handle(), ticket).await.is_err(), "{code}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn host_init_refuses_an_oversized_user_agent() {
    assert!(commands::host_init(&"a".repeat(1025)).is_err());
    assert!(commands::host_init("bad\nvalue").is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn perr_failures_carry_the_text_the_typescript_route_shows() {
    let server = serve("500 Internal Server Error", r#"{"error":"Synthetic failure"}"#, false).await;
    let harness = setup(&server.origin, json!({}), None);
    let ticket = harness.reserve().await;
    harness.execute(&ticket).await;
    let status = harness.settled(&ticket).await;
    assert_eq!(status.phase, Phase::Failed);
    let failure = status.failure.clone().unwrap();
    assert_eq!((failure.category.as_str(), failure.http_status, failure.sdk_message.as_deref()), ("http", Some(500), Some("Synthetic failure")));
    assert!(status.diagnostics_available);
    assert_eq!((status.output, server.requests()), (RouteOutput::None, 1));

    // A refused connection is the WebView fetch TypeError in the TS route.
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let origin = format!("http://127.0.0.1:{}", closed.local_addr().unwrap().port());
    drop(closed);
    let harness = setup(&origin, json!({}), None);
    let ticket = harness.reserve().await;
    harness.execute(&ticket).await;
    let status = harness.settled(&ticket).await;
    let failure = status.failure.unwrap();
    assert_eq!((status.phase, failure.category.as_str(), failure.sdk_message.as_deref()), (Phase::Failed, "network", Some("Failed to fetch")));

    for (body, message) in [
        (reply(json!(42)), "text.indexOf is not a function"),
        (reply(json!(["system:"])), "text.substring is not a function"),
        ("null".into(), "Cannot read properties of null (reading 'message')"),
    ] {
        let server = serve("200 OK", &body, false).await;
        let harness = setup(&server.origin, json!({}), None);
        let ticket = harness.reserve().await;
        harness.execute(&ticket).await;
        let status = harness.settled(&ticket).await;
        assert_eq!((status.phase, status.output), (Phase::Failed, RouteOutput::None));
        assert_eq!(status.failure.unwrap().sdk_message.as_deref(), Some(message));
        assert_eq!(server.requests(), 1);
    }

    // The native parser reports a bounded cause and byte position, without copying the response body.
    let server = serve("200 OK", "{\"synthetic-private-field\":", false).await;
    let harness = setup(&server.origin, json!({}), None);
    let ticket = harness.reserve().await;
    harness.execute(&ticket).await;
    let status = harness.settled(&ticket).await;
    assert_eq!((status.phase, status.output), (Phase::Failed, RouteOutput::None));
    let failure = status.failure.unwrap();
    assert_eq!((failure.category.as_str(), failure.field.as_deref(), failure.stage.as_deref()),
        ("decode", Some("syntax"), Some("buffered-json")));
    let message = failure.sdk_message.unwrap();
    let position = message.strip_prefix("Invalid JSON at byte ").expect("native syntax diagnostic").parse::<usize>().unwrap();
    assert!(position > 0 && position <= "{\"synthetic-private-field\":".len());
    assert!(!message.contains("synthetic-private-field"));
    assert_eq!((server.requests(), harness.messages().len()), (1, 3));

    // Policy A: the fallback row is committed and the operation still fails.
    let server = serve("200 OK", &reply(json!([" alpha ", null, 7, ["nested", "tail"]])), false).await;
    let harness = setup(&server.origin, json!({}), None);
    let ticket = harness.reserve().await;
    harness.execute(&ticket).await;
    let status = harness.settled(&ticket).await;
    let failure = status.failure.clone().unwrap();
    assert_eq!((status.phase.clone(), status.output), (Phase::Failed, RouteOutput::Committed));
    assert_eq!((failure.category.as_str(), failure.field.as_deref(), failure.stage.as_deref(), failure.sdk_message), ("type", Some("trim"), Some("output"), Some("data.trim is not a function".into())));
    let proof = commands::proof(harness.handle(), ticket, None).await.unwrap();
    assert_eq!((proof.at, proof.failed_with_output, proof.output_message_id), ("failed", true, Some(status.message_id)));
    assert_eq!(harness.messages().last().unwrap()["data"], "alpha ,,7,nested,tail");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_status_waits_for_the_live_diagnostic_publication() {
    let server = serve("500 Internal Server Error", r#"{"error":"Synthetic publication failure"}"#, true).await;
    let harness = setup(&server.origin, json!({}), None);
    let ticket = harness.reserve().await;
    let gate = commands::hold_publication(&ticket);
    harness.execute(&ticket).await;
    server.requested(1).await;
    server.release();
    bounded(gate.reached.notified()).await;
    assert_eq!(with_store(harness.state(), |store| store.generation_status(&ticket)).unwrap().phase, Phase::Failed);
    let requested = ticket.clone();
    let app = harness.handle();
    let mut read = tokio::spawn(async move { commands::status(app, requested, None).await });
    tokio::select! {
        biased;
        result = &mut read => panic!("terminal status escaped before its diagnostic: {result:?}"),
        _ = bounded(gate.observed.notified()) => {}
    }
    assert!(!read.is_finished());
    gate.release.notify_one();
    let status = bounded(read).await.unwrap().unwrap();
    assert_eq!(status.phase, Phase::Failed);
    assert_eq!(status.failure.unwrap().sdk_message.as_deref(), Some("Synthetic publication failure"));
    assert!(status.diagnostics_available);
    assert_eq!(server.requests(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn preparation_failure_is_terminal_without_dispatch_or_output() {
    let server = serve("200 OK", &reply(json!("Never requested.")), false).await;
    let harness = setup(&server.origin, json!({}), Some(json!([
        {"role":"user", "data":"<|endoftext|>", "chatId":"synthetic-special-token"}
    ])));
    let ticket = harness.reserve().await;
    let status = harness.execute(&ticket).await;
    assert_eq!((status.phase, status.dispatch, status.output), (Phase::Failed, RouteDispatch::None, RouteOutput::None));
    let failure = status.failure.unwrap();
    assert_eq!((failure.category.as_str(), failure.field.as_deref(), failure.stage.as_deref()),
        ("tokenizer", Some("disallowed_special_token"), Some("preparation")));
    assert!(!status.preparation_ready && !status.diagnostics_available);
    assert_eq!(harness.execute(&ticket).await.phase, Phase::Failed);
    assert_eq!(server.requests(), 0);
    assert_eq!(harness.messages().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retained_terminal_ticket_is_readable_without_the_host_registry() {
    let server = serve("200 OK", &reply(json!("Persisted terminal reply.")), false).await;
    let harness = setup(&server.origin, json!({}), None);
    let ticket = harness.reserve().await;
    harness.execute(&ticket).await;
    let completed = harness.settled(&ticket).await;
    bounded(async { while !commands::finished(&ticket) { tokio::task::yield_now().await; } }).await;
    commands::forget_host(&ticket);
    let status = harness.status(&ticket).await;
    assert_eq!((status.character_id.as_str(), status.conversation_id.as_str(), status.phase),
        ("character", "conversation", Phase::Completed));
    let proof = commands::proof(harness.handle(), ticket.clone(), None).await.unwrap();
    assert_eq!(proof.output_message_id, Some(completed.message_id));
    assert_eq!(harness.execute(&ticket).await.phase, Phase::Completed);
    assert_eq!(server.requests(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cana_cancel_during_the_provider_wait_records_each_reason() {
    for name in ["user", "edit", "plugin", "authority", "lifecycle"] {
        let reason: CancelReason = serde_json::from_value(json!(name)).unwrap();
        let server = serve("200 OK", &reply(json!("Never delivered.")), true).await;
        let harness = setup(&server.origin, json!({}), None);
        let ticket = harness.reserve().await;
        harness.execute(&ticket).await;
        server.requested(1).await;
        assert_eq!(harness.issued(&ticket).await.dispatch, RouteDispatch::Issued, "{name}");
        let cancelled = commands::cancel(harness.handle(), ticket.clone(), reason).await.unwrap();
        assert_eq!(cancelled.cancel_decision, Some(reason), "{name}");
        let status = harness.settled(&ticket).await;
        assert_eq!((status.phase.clone(), status.cancel_decision, status.output), (Phase::Cancelled, Some(reason), RouteOutput::None), "{name}");
        assert_eq!(serde_json::to_value(&status).unwrap()["cancelDecision"], name);
        server.closed_by_client(1).await;
        assert_eq!(server.requests(), 1, "{name}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn canb_cancel_before_execute_sends_no_request() {
    let server = serve("200 OK", &reply(json!("Never requested.")), false).await;
    let harness = setup(&server.origin, json!({}), None);
    let ticket = harness.reserve().await;
    let cancelled = commands::cancel(harness.handle(), ticket.clone(), CancelReason::User).await.unwrap();
    assert_eq!(cancelled.phase, Phase::Cancelled);
    assert_eq!(harness.execute(&ticket).await.phase, Phase::Cancelled);
    bounded(async { while !commands::finished(&ticket) { tokio::task::yield_now().await; } }).await;
    assert_eq!((harness.status(&ticket).await.dispatch, server.requests()), (RouteDispatch::None, 0));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn device_maintenance_during_the_provider_wait_cancels_without_holding_admission() {
    let server = serve("200 OK", &reply(json!("Never delivered.")), true).await;
    let harness = setup(&server.origin, json!({}), None);
    let ticket = harness.reserve().await;
    harness.execute(&ticket).await;
    server.requested(1).await;
    harness.issued(&ticket).await;
    let app = harness.handle();
    // Maintenance drains admitted operations first, so it returns only if the owner holds none across the wait.
    let maintenance = bounded(tokio::task::spawn_blocking(move || app.state::<PersistentStoreState>().acquire_raw_capture().unwrap())).await.unwrap();
    server.closed_by_client(1).await;
    drop(maintenance);
    let status = harness.settled(&ticket).await;
    assert_eq!((status.phase, status.cancel_decision, status.output), (Phase::Cancelled, Some(CancelReason::Lifecycle), RouteOutput::None));
    assert_eq!(server.requests(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn renderer_document_reset_during_the_provider_wait_keeps_the_owner_and_its_listing() {
    for overlap in [false, true] {
        let server = serve("200 OK", &reply(json!("Delivered after reset.")), true).await;
        let harness = setup(&server.origin, json!({}), None);
        let ticket = harness.reserve().await;
        harness.execute(&ticket).await;
        server.requested(1).await;
        harness.issued(&ticket).await;
        let checked = commands::MAINTENANCE_CHECKED.load(Ordering::Acquire);
        let blocker = overlap.then(|| harness.state().admit_renderer_operation().unwrap());
        let app = harness.handle();
        let reset = tokio::task::spawn_blocking(move || app.state::<PersistentStoreState>().reset_renderer_session().unwrap());
        if overlap {
            bounded(async { while !harness.state().maintenance_active() { tokio::task::yield_now().await; } }).await;
            // Deliver the response while reset is still draining a renderer operation.
            server.release();
            bounded(async { while !harness.state().native_job_waiting_for_reset() { tokio::task::yield_now().await; } }).await;
            assert!(!reset.is_finished());
        }
        drop(blocker);
        bounded(reset).await.unwrap();
        if !overlap {
            bounded(async { while commands::MAINTENANCE_CHECKED.load(Ordering::Acquire) == checked { tokio::task::yield_now().await; } }).await;
            commands::forget_host(&ticket);
            let listed = commands::active(harness.handle()).await.unwrap();
            assert_eq!(listed.iter().map(|status| (&status.ticket, status.character_id.as_str(), status.conversation_id.as_str(), &status.phase)).collect::<Vec<_>>(),
                [(&ticket, "character", "conversation", &Phase::Running)]);
            server.release();
        }
        let status = harness.settled(&ticket).await;
        assert_eq!((status.phase, status.cancel_decision), (Phase::Completed, None), "overlap={overlap}");
        assert_eq!((server.requests(), server.dropped()), (1, 0));
        let rows = harness.messages();
        assert_eq!(rows.len(), 4);
        assert_eq!(rows.last().unwrap()["data"], "Delivered after reset.");
        assert!(harness.active().await.is_empty());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn proof_at_issued_names_only_the_pre_dispatch_units() {
    let mut messages = fixture()["messages"].clone();
    messages[2].as_object_mut().unwrap().remove("chatId");
    let server = serve("200 OK", &reply(json!("After the proof.")), true).await;
    let harness = setup(&server.origin, json!({}), Some(messages));
    let original = harness.lease(harness.revision());
    let ticket = harness.reserve().await;
    let witness = commands::capture_witness(harness.handle(), ticket.clone(), serde_json::from_value(original.clone()).unwrap(), vec![2]).await.unwrap();
    let witness = serde_json::to_value(&witness).unwrap();
    assert_eq!(witness["rows"], json!([{ "originalIndex": 2, "rowHash": witness["rows"][0]["rowHash"], "idState": "missing" }]));
    harness.execute(&ticket).await;
    server.requested(1).await;
    assert_eq!(harness.issued(&ticket).await.phase, Phase::Running);
    let current = harness.lease(harness.revision());
    // The witness and both leases cross IPC back unchanged.
    let input: NormalizationInput = serde_json::from_value(json!({ "witness": witness, "original": original, "current": current })).unwrap();
    let proof = commands::proof(harness.handle(), ticket.clone(), Some(input)).await.unwrap();
    assert_eq!((proof.at, proof.affected_unit_keys.clone(), proof.output_message_id.clone()), ("issued", affected_keys(), None));
    assert!(proof.receipts.normalization.is_some() && proof.receipts.output.is_none() && !proof.mapping_unavailable && !proof.failed_with_output);
    assert_effect_receipts(&proof);
    let deltas = serde_json::to_value(proof.id_deltas.clone().unwrap()).unwrap();
    let assigned = harness.messages()[2]["chatId"].as_str().unwrap().to_owned();
    assert_eq!(deltas, json!([{ "originalIndex": 2, "currentIndex": 2, "originalRowHash": deltas[0]["originalRowHash"], "originalIdState": "missing", "assignedId": assigned }]));
    let unmapped = commands::proof(harness.handle(), ticket.clone(), None).await.unwrap();
    assert_eq!((unmapped.id_deltas, unmapped.mapping_unavailable), (None, true));

    server.release();
    assert_eq!(harness.settled(&ticket).await.phase, Phase::Completed);
    let terminal = commands::proof(harness.handle(), ticket, None).await.unwrap();
    assert_effect_receipts(&terminal);
    assert_eq!((terminal.at, terminal.affected_unit_keys), ("completed", affected_keys()));
    assert!(terminal.receipts.output.is_some() && terminal.output_message_id.is_some());
    assert_eq!(server.requests(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn variable_receipts_reach_issued_failed_and_cancelled_command_proofs() {
    use super::{inputs::PreparationError, operation::Start, preparation_effects::preparation_variables};
    use crate::persistent_store::generation::{DispatchClaim, TerminalDecision};
    use risunest_ir::{stage::Effect, Budget, Limits, Text, Value as Ir};
    // A variable receipt names the scriptstate and author note units it may have written.
    let variable_keys = [r#"["conversation","character","conversation","scriptstate"]"#, r#"["conversation","character","conversation","note"]"#];
    for written in [false, true] {
        for (disposition, memory) in [("issued", Some("Synthetic proof memory.")), ("issued", None),
            ("failed", None), ("cancelled", Some("Synthetic proof memory."))] {
            let mut messages = fixture()["messages"].clone();
            messages[2].as_object_mut().unwrap().remove("chatId");
            let harness = setup("http://localhost", json!({}), Some(messages));
            let original = harness.lease(harness.revision());
            let ticket = harness.reserve().await;
            let witness = commands::capture_witness(harness.handle(), ticket.clone(),
                serde_json::from_value(original.clone()).unwrap(), vec![2]).await.unwrap();
            let witness = serde_json::to_value(witness).unwrap();
            let (mut expected_keys, expected_receipts, variable_receipt) = with_store_mut(harness.state(), |store| {
                let owner = match store.generation_start_preparation(&ticket)? {
                    Start::Started { owner, .. } => owner,
                    other => panic!("{other:?}"),
                };
                store.generation_ensure_history_ids(&owner).unwrap();
                let snapshot = store.generation_owned_preparation_snapshot(&owner)?.unwrap();
                let budget = Budget::new(Limits { max_owned_bytes: 1 << 20, max_json_bytes: 1 << 20, max_text_units: 1 << 18,
                    max_values: 1 << 16, max_depth: 128, max_work: 1 << 20 },
                    std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false))).unwrap();
                let effects = if written { vec![Effect::ChatVariable {
                    name: Text::from_str("__internal_ka_synthetic", &budget).unwrap(),
                    value: Ir::Text(Text::from_str("true", &budget).unwrap()),
                }] } else { Vec::new() };
                let (plan, converted) = preparation_variables(&snapshot, &effects);
                converted.unwrap();
                let revision = store.revision()?;
                let before = store.generation_effects_evidence(&ticket)?;
                assert!(before.is_none());
                if disposition == "failed" {
                    let status = store.generation_fail_preparation_with_variables(&owner, PreparationError::ContextLimit, &plan)?;
                    assert_eq!((status.phase, status.preparation_error), (Phase::Failed, Some(PreparationError::ContextLimit)));
                    assert_eq!(store.revision()?, revision + i64::from(written));
                } else {
                    let lifecycle = uuid::Uuid::new_v4().to_string();
                    store.generation_mark_ready_owned(&owner, &lifecycle)?;
                    store.generation_send_variables(&owner, &lifecycle, &plan)?;
                    assert_eq!(store.revision()?, revision + i64::from(written));
                    store.generation_send_variables(&owner, &lifecycle, &plan)?;
                    assert_eq!(store.revision()?, revision + i64::from(written));
                    store.generation_send_memory(&owner, &lifecycle, memory)?;
                    assert!(matches!(store.generation_claim_dispatch(&owner, &lifecycle)?, DispatchClaim::Issued { .. }));
                    if disposition == "cancelled" {
                        store.generation_settle_owned(&owner, TerminalDecision::Cancelled { reason: CancelReason::User })?;
                    }
                }
                let evidence = store.generation_effects_evidence(&ticket)?.unwrap();
                let mut expected_keys = effect_keys();
                if memory.is_none() { expected_keys.pop(); }
                if written { for key in variable_keys.into_iter().rev() { expected_keys.insert(2, key.into()); } }
                let expected_receipts = 1 + usize::from(written) + usize::from(memory.is_some());
                assert_eq!(evidence.affected_unit_keys, expected_keys);
                assert_eq!(evidence.receipts.len(), expected_receipts);
                let variable_receipt = if written {
                    let durable: (String, String, i64) = store.library_rows().query_row(
                        "SELECT request_id,digest,revision FROM lww_requests WHERE revision=?1", [revision + 1],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
                    assert_eq!(evidence.receipts[1].revision, revision + 1);
                    assert_eq!((&evidence.receipts[1].request_id, &evidence.receipts[1].digest, evidence.receipts[1].revision),
                        (&durable.0, &durable.1, durable.2));
                    assert_ne!(durable.0, evidence.receipts[0].request_id);
                    if memory.is_some() { assert_ne!(durable.0, evidence.receipts[2].request_id); }
                    Some(json!({ "requestId": durable.0, "intentDigest": durable.1, "receiptRevision": durable.2 }))
                } else { None };
                let conversation = store.read_conversation("character", "conversation", None)?.unwrap().value;
                if written { assert_eq!(conversation["scriptstate"]["$__internal_ka_synthetic"], "true"); }
                else { assert!(conversation["scriptstate"]["$__internal_ka_synthetic"].is_null()); }
                Ok((expected_keys, expected_receipts, variable_receipt))
            }).unwrap();
            let revision = harness.revision();
            let current = harness.lease(revision);
            let input = serde_json::from_value(json!({ "witness": witness, "original": original, "current": current })).unwrap();
            let proof = commands::proof(harness.handle(), ticket.clone(), Some(input)).await.unwrap();
            expected_keys.push(messages_key());
            assert!(!proof.mapping_unavailable);
            assert_eq!(proof.id_deltas.as_ref().unwrap().len(), 1);
            assert_eq!((proof.at, proof.affected_unit_keys.clone(), proof.receipts.effects.len()),
                (disposition, expected_keys, expected_receipts));
            assert!(proof.receipts.output.is_none() && proof.output_message_id.is_none());
            if let Some(receipt) = variable_receipt {
                assert_eq!(proof.receipts.effects[1], receipt);
            }
            assert_effect_receipts(&proof);
            let repeated = commands::proof(harness.handle(), ticket, None).await.unwrap();
            assert_eq!(repeated.receipts.effects, proof.receipts.effects);
            assert_eq!(harness.revision(), revision);
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn status_after_observes_dispatch_between_two_renderer_calls() {
    let server = serve("200 OK", &reply(json!("After observation.")), true).await;
    let harness = setup(&server.origin, json!({}), None);
    let ticket = harness.reserve().await;
    let before = harness.status(&ticket).await;
    harness.execute(&ticket).await;
    server.requested(1).await;
    let current = harness.issued(&ticket).await;
    assert_eq!(current.dispatch, RouteDispatch::Issued);
    // Keep the reply held: there is no later phase change to rescue a missed dispatch.
    let waiting = commands::observe_status_wait(&ticket);
    let observed = tokio::select! {
        biased;
        _ = waiting.notified() => panic!("missed dispatch change and entered long poll"),
        result = bounded(commands::status_after(harness.handle(), ticket.clone(), Some(commands::MAX_STATUS_WAIT_MS), Some((&before).into()))) => result.unwrap(),
    };
    assert_eq!((observed.phase, observed.dispatch), (Phase::Running, RouteDispatch::Issued));
    assert_eq!(server.requests(), 1);
    server.release();
    assert_eq!(harness.settled(&ticket).await.phase, Phase::Completed);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn command_restart_after_issued_reports_interrupted_and_never_redispatches() {
    const CHILD: &str = "RISUNEST_COMMAND_RESTART_ROOT";
    const TICKET: &str = "RISUNEST_COMMAND_RESTART_TICKET";
    if let Some(root) = std::env::var_os(CHILD) {
        let store = PersistentStore::open(std::path::Path::new(&root)).unwrap();
        let app = tauri::test::mock_builder().build(tauri::test::mock_context(tauri::test::noop_assets())).unwrap();
        app.manage(PersistentStoreState::with_test_store(store));
        commands::host_init(USER_AGENT).unwrap();
        let ticket: Ticket = serde_json::from_str(&std::env::var(TICKET).unwrap()).unwrap();
        // A direct read must work even when startup reconciliation already made the ticket terminal.
        let status = commands::status(app.handle().clone(), ticket.clone(), None).await.unwrap();
        assert_eq!((status.phase, status.dispatch, status.character_id.as_str(), status.conversation_id.as_str()),
            (Phase::Interrupted, RouteDispatch::Uncertain, "character", "conversation"));
        assert!(!status.diagnostics_available);
        assert!(commands::active(app.handle().clone()).await.unwrap().is_empty());
        let proof = commands::proof(app.handle().clone(), ticket.clone(), None).await.unwrap();
        assert_eq!((proof.at, proof.affected_unit_keys.clone(), proof.output_message_id.clone()), ("interrupted", effect_keys(), None));
        assert_effect_receipts(&proof);
        assert_eq!(commands::execute(app.handle().clone(), ticket,).await.unwrap().phase, Phase::Interrupted);
        return;
    }
    let server = serve("200 OK", &reply(json!("Lost reply.")), true).await;
    let harness = setup(&server.origin, json!({}), None);
    let ticket = harness.reserve().await;
    let requested = ticket.clone();
    let app = harness.handle();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let worker = std::thread::spawn(move || {
        // Dropping this runtime stops the command supervisor and its owner without cancelling the journal.
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
        runtime.block_on(async {
            commands::execute(app, requested).await.unwrap();
            stopped.await.unwrap();
        });
    });
    server.requested(1).await;
    harness.issued(&ticket).await;
    stop.send(()).unwrap();
    bounded(tokio::task::spawn_blocking(move || worker.join().unwrap())).await.unwrap();
    server.closed_by_client(1).await;
    commands::forget_host(&ticket);
    let executable = std::env::current_exe().unwrap();
    let name = format!("{}::command_restart_after_issued_reports_interrupted_and_never_redispatches", module_path!().split_once("::").unwrap().1);
    let root = harness._directory.path().to_owned();
    let encoded = serde_json::to_string(&ticket).unwrap();
    let child = bounded(tokio::task::spawn_blocking(move || std::process::Command::new(executable)
        .args(["--exact", &name, "--nocapture"]).env(CHILD, root).env(TICKET, encoded).status().unwrap())).await.unwrap();
    assert!(child.success(), "restarted command process failed: {child}");
    assert_eq!(server.requests(), 1);
    assert_eq!(harness.messages().len(), 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn active_listing_does_not_retarget_an_old_ticket_to_a_replacement_library() {
    let server = serve("200 OK", &reply(json!("Discarded old-library reply.")), true).await;
    let harness = setup(&server.origin, json!({}), None);
    let ticket = harness.reserve().await;
    harness.execute(&ticket).await;
    server.requested(1).await;
    harness.issued(&ticket).await;
    let original = with_store(harness.state(), |store| store.generation_target(&ticket)).unwrap();
    with_store_mut(harness.state(), |store| {
        let fixture = fixture();
        let mut character = fixture["character"].clone();
        let mut conversation = fixture["conversation"].clone();
        conversation["message"] = fixture["messages"].clone();
        character["chats"] = json!([conversation]);
        let stage = store.replace_begin()?;
        store.replace_put_root(&stage.staging_id, &fixture["root"])?;
        store.replace_put_presets(&stage.staging_id, &[fixture["preset"].clone()])?;
        store.replace_add_characters(&stage.staging_id, &[character])?;
        store.replace_commit(&stage.staging_id, Some(store.revision()?))?;
        assert_ne!(store.library_identity()?, original.library_identity);
        Ok(())
    }).unwrap();
    commands::forget_host(&ticket);
    assert!(harness.active().await.is_empty());
    assert_eq!(with_store(harness.state(), |store| store.generation_target(&ticket)).unwrap(), original);
    server.release();
    assert_eq!(harness.settled(&ticket).await.phase, Phase::Stale);
    assert!(commands::proof(harness.handle(), ticket, None).await.is_err());
    assert_eq!((server.requests(), harness.messages().len()), (1, 3));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_preparation_admission_wakes_a_concurrent_terminal_reader() {
    let server = serve("200 OK", &reply(json!("Never requested.")), false).await;
    let harness = setup(&server.origin, json!({}), None);
    let ticket = harness.reserve().await;
    let gate = commands::hold_preparation(&ticket);
    let publication = commands::hold_publication(&ticket);
    let app = harness.handle();
    let requested = ticket.clone();
    let execute = tokio::spawn(async move { commands::execute(app, requested).await });
    bounded(gate.reached.notified()).await;
    let maintenance = harness.state().acquire_raw_capture().unwrap();
    gate.release.notify_one();
    // The actual store admission rejects preparation while maintenance owns the gate.
    bounded(gate.failed.notified()).await;
    drop(maintenance);
    assert!(with_store(harness.state(), |store| store.generation_execution(&ticket)).unwrap().is_none());
    let app = harness.handle();
    let requested = ticket.clone();
    let mut cancel = tokio::spawn(async move { commands::cancel(app, requested, CancelReason::User).await });
    tokio::select! {
        biased;
        result = &mut cancel => panic!("terminal publication escaped preparation: {result:?}"),
        _ = bounded(publication.observed.notified()) => {}
    }
    gate.release_failure.notify_one();
    assert!(bounded(execute).await.unwrap().is_err());
    let status = bounded(cancel).await.unwrap().unwrap();
    assert_eq!((status.phase, status.dispatch, status.cancel_decision), (Phase::Cancelled, RouteDispatch::None, None));
    assert_eq!((server.requests(), harness.messages().len()), (0, 3));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropping_the_execute_caller_before_preparation_releases_its_publication_claim() {
    let server = serve("200 OK", &reply(json!("Never requested.")), false).await;
    let harness = setup(&server.origin, json!({}), None);
    let ticket = harness.reserve().await;
    let gate = commands::hold_preparation(&ticket);
    let publication = commands::hold_publication(&ticket);
    let app = harness.handle();
    let requested = ticket.clone();
    let execute = tokio::spawn(async move { commands::execute(app, requested).await });
    bounded(gate.reached.notified()).await;
    let app = harness.handle();
    let requested = ticket.clone();
    let mut cancel = tokio::spawn(async move { commands::cancel(app, requested, CancelReason::User).await });
    tokio::select! {
        biased;
        result = &mut cancel => panic!("terminal publication escaped preparation: {result:?}"),
        _ = bounded(publication.observed.notified()) => {}
    }
    execute.abort();
    assert!(bounded(execute).await.unwrap_err().is_cancelled());
    let status = bounded(cancel).await.unwrap().unwrap();
    assert_eq!((status.phase, status.dispatch), (Phase::Cancelled, RouteDispatch::None));
    assert_eq!(server.requests(), 0);
}
