use super::{
    inputs::{OllamaIntent, PreparationError},
    operation::{ActiveGeneration, DispatchState, Owner, OutputState, OwnerStatus, Phase, Start, Target, Ticket},
    owner::{self, OwnerExecution, OwnerHost, OwnerLimits, OwnerStore, ReadyOllama, RegistryLimits},
    service::{self, GenerationError, ReadyPreparation},
};
use crate::persistent_store::{
    generation::{self, CancelReason, DispatchClaim, FailureDescriptor},
    lww, sync_selection::{BindingSelectionChange, SyncTarget}, ConversationMutation, PersistentStore, RootMutation, StoreError, StoreResult, WorkingSetCommit,
};
use loopback::{serve, Reply};
use risunest_sync_wire::unit::UnitKey;
use serde_json::{json, value::RawValue, Value};
use std::{collections::VecDeque, process::Command, sync::{mpsc, Arc, Mutex}};
use tokio::sync::{oneshot, Notify};

const USER_AGENT: &str = "Mozilla/5.0 (Synthetic) RisuNest-owner-test";
const JOURNAL_KEY: &str = "native-generation-journal-v1";
// The request body the TS oracle observed for the admitted ordinary profile of ollama-text.json.
const ORACLE_REQUEST: &str = concat!(r#"{"model":"synthetic-model","messages":["#,
    r#"{"role":"system","content":"Synthetic main."},{"role":"system","content":"Synthetic description."},"#,
    r#"{"role":"system","content":"Synthetic persona."},{"role":"system","content":"[Start a new chat]"},"#,
    r#"{"role":"assistant","content":"Synthetic greeting."},{"role":"user","content":"First question."},"#,
    r#"{"role":"assistant","content":"First answer."},{"role":"user","content":"Second question."},"#,
    r#"{"role":"system","content":"Synthetic global note."},{"role":"system","content":"Synthetic author note."}],"stream":false}"#);

mod loopback {
    use std::{collections::VecDeque, sync::{Arc, Mutex}, time::Duration};
    use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::{TcpListener, TcpStream}, sync::Notify, task::{JoinHandle, JoinSet}};

    #[derive(Debug, Clone)]
    pub struct Request { pub path: String, pub headers: Vec<(String, String)>, pub body: Vec<u8> }
    impl Request {
        pub fn header(&self, name: &str) -> Option<&str> { self.headers.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str()) }
    }
    pub enum Reply { Respond { status: &'static str, body: Vec<u8> }, HoldHeaders, HoldBody(Vec<u8>), Gated(Arc<Notify>, Box<Reply>) }
    #[derive(Default)]
    struct Shared { requests: Mutex<Vec<Request>>, seen: Notify }
    pub struct Server { pub origin: String, shared: Arc<Shared>, task: JoinHandle<()> }
    impl Drop for Server { fn drop(&mut self) { self.task.abort(); } }
    impl Server {
        pub fn requests(&self) -> Vec<Request> { self.shared.requests.lock().unwrap().clone() }
        // A guard against a hung owner, not a timing assertion.
        pub async fn seen(&self) {
            tokio::time::timeout(Duration::from_secs(30), self.shared.seen.notified()).await.expect("loopback request was not received");
        }
    }
    // Each accepted connection takes the next reply; connections beyond the list are closed unanswered.
    pub async fn serve(replies: Vec<Reply>) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        let mut replies = VecDeque::from(replies);
        let shared = Arc::new(Shared::default());
        let handlers = shared.clone();
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            while let Ok((stream, _)) = listener.accept().await {
                while connections.try_join_next().is_some() {}
                connections.spawn(handle(stream, replies.pop_front(), handlers.clone()));
            }
        });
        Server { origin, shared, task }
    }
    async fn handle(mut stream: TcpStream, reply: Option<Reply>, shared: Arc<Shared>) {
        let Some(request) = read_request(&mut stream).await else { return };
        shared.requests.lock().unwrap().push(request);
        shared.seen.notify_one();
        let Some(mut reply) = reply else { return };
        while let Reply::Gated(release, next) = reply { release.notified().await; reply = *next; }
        match reply {
            Reply::Respond { status, body } => {
                let head = format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                if stream.write_all(head.as_bytes()).await.is_ok() && stream.write_all(&body).await.is_ok() { let _ = stream.shutdown().await; }
            }
            Reply::HoldHeaders => closed(&mut stream).await,
            Reply::HoldBody(part) => {
                let mut head = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n".to_vec();
                head.extend_from_slice(format!("{:x}\r\n", part.len()).as_bytes()); head.extend_from_slice(&part); head.extend_from_slice(b"\r\n");
                if stream.write_all(&head).await.is_ok() && stream.flush().await.is_ok() { closed(&mut stream).await; }
            }
            Reply::Gated(..) => unreachable!(),
        }
    }
    // The client sends nothing after its request, so this read returns only once it closes.
    async fn closed(stream: &mut TcpStream) { let _ = stream.read(&mut [0; 1]).await; }
    async fn read_request(stream: &mut TcpStream) -> Option<Request> {
        let mut data = Vec::new(); let mut block = [0; 4096];
        let head = loop {
            if let Some(index) = data.windows(4).position(|window| window == b"\r\n\r\n") { break index + 4; }
            let read = stream.read(&mut block).await.ok()?;
            if read == 0 { return None; }
            data.extend_from_slice(&block[..read]);
        };
        let text = std::str::from_utf8(&data[..head]).ok()?;
        let mut lines = text.split("\r\n");
        let path = lines.next()?.split(' ').nth(1)?.to_owned();
        let headers = lines.filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned())).collect::<Vec<_>>();
        let length = headers.iter().find(|(name, _)| name == "content-length").map_or(Some(0), |(_, value)| value.parse::<usize>().ok())?;
        let mut body = data[head..].to_vec();
        while body.len() < length {
            let read = stream.read(&mut block).await.ok()?;
            if read == 0 { return None; }
            body.extend_from_slice(&block[..read]);
        }
        Some(Request { path, headers, body })
    }
}

fn reply(content: &str, thinking: Option<&str>) -> Reply {
    let thinking = thinking.map(|value| format!(r#","thinking":{value}"#)).unwrap_or_default();
    Reply::Respond { status: "200 OK", body: format!(
        r#"{{"model":"synthetic-model","created_at":"2026-10-25T00:00:00Z","message":{{"role":"assistant","content":{content}{thinking}}},"done":true}}"#).into_bytes() }
}

fn gated(release: &Arc<Notify>, reply: Reply) -> Reply { Reply::Gated(release.clone(), Box::new(reply)) }

#[derive(serde::Deserialize)]
struct ResponseCase {
    id: String,
    #[serde(rename = "oracleScope")]
    oracle_scope: String,
    content: Box<RawValue>,
    #[serde(default)]
    thinking: Option<Box<RawValue>>,
    route: String,
    output: Box<RawValue>,
    #[serde(rename = "nativeOutput")]
    native_output: Option<String>,
    #[serde(rename = "continue", default)]
    continuation: bool,
    #[serde(rename = "nativeUnsupported", default)]
    native_unsupported: Option<String>,
    #[serde(rename = "midflightDefaults", default)]
    midflight_defaults: Option<Value>,
}

// Raw values keep isolated surrogate escapes exactly as the fixture writes them.
fn raw_response_cases() -> Vec<Box<RawValue>> {
    #[derive(serde::Deserialize)]
    struct Fixture { cases: Vec<Box<RawValue>> }
    serde_json::from_str::<Fixture>(include_str!("../../../tests/fixtures/native-generation/ollama-owner-response.json")).unwrap().cases
}

fn response_cases() -> Vec<ResponseCase> {
    raw_response_cases().into_iter().map(|case| serde_json::from_str::<ResponseCase>(case.get()).unwrap())
        .filter(|case| !case.continuation).collect()
}

fn response_case(id: &str) -> ResponseCase { response_cases().into_iter().find(|case| case.id == id).unwrap() }

fn text_fixture() -> Value {
    serde_json::from_str(include_str!("../../../tests/fixtures/native-generation/ollama-text.json")).unwrap()
}

enum Fault {
    Unavailable,
    // Waits before the segment starts, after telling the test it was reached.
    Hold { entered: oneshot::Sender<()>, release: mpsc::Receiver<()> },
    // Tells the test once the output commit is admitted under the segment gate.
    Admitted(oneshot::Sender<()>),
    // Commits, then loses the journal's resolution and the reply, as a crash after the library commit would.
    LoseReply,
    // Interrupts after the intent is durable, then leaves it quarantined for exact recovery.
    InterruptedOffer,
    // Runs between the output snapshot and its commit, as another writer would.
    Race(Box<dyn FnOnce() + Send>),
}

struct TestAccess { store: Mutex<PersistentStore>, faults: Mutex<VecDeque<Option<Fault>>> }

fn journal(store: &PersistentStore) -> String {
    store.device_store().unwrap().connection().query_row("SELECT value FROM device_settings WHERE key=?1", [JOURNAL_KEY], |row| row.get(0)).unwrap()
}

impl OwnerStore for TestAccess {
    fn segment(&self, work: &mut dyn FnMut(&mut PersistentStore)) -> StoreResult<()> {
        let fault = self.faults.lock().unwrap().pop_front().flatten();
        if let Some(Fault::Unavailable) = fault { return Err(StoreError::Validation { message: "synthetic-store-unavailable".into() }); }
        let fault = match fault {
            Some(Fault::Hold { entered, release }) => { let _ = entered.send(()); release.recv().unwrap(); None }
            fault => fault,
        };
        let mut store = self.store.lock().unwrap();
        if matches!(fault, Some(Fault::InterruptedOffer)) {
            lww::BEFORE_HELD_COMMIT.with(|hook| *hook.borrow_mut() = Some(Box::new(|| panic!("synthetic interrupted offer"))));
            assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| work(&mut store))).is_err());
            let journal: Value = serde_json::from_str(&journal(&store)).unwrap();
            let request_id = journal["journal"]["records"].as_object().unwrap().values()
                .find_map(|record| record["attempt"]["request_id"].as_str()).unwrap();
            store.device_store().unwrap().connection().execute(
                "INSERT INTO lww_intent_failures(request_id,failures,error,quarantined) VALUES(?1,3,'synthetic',1)", [request_id]).unwrap();
            return Err(StoreError::Validation { message: "synthetic-interrupted-offer".into() });
        }
        let lost = match fault {
            Some(Fault::Race(racer)) => {
                generation::BEFORE_CHECKPOINT_COMMIT.with(|hook| *hook.borrow_mut() = Some(racer as Box<dyn FnOnce()>));
                None
            }
            Some(Fault::Admitted(admitted)) => {
                lww::BEFORE_HELD_COMMIT.with(|hook| *hook.borrow_mut() = Some(Box::new(move || { let _ = admitted.send(()); })));
                None
            }
            Some(Fault::LoseReply) => {
                let observer = store.open_native_job_store().unwrap();
                let captured = Arc::new(Mutex::new(None));
                let slot = captured.clone();
                lww::BEFORE_HELD_COMMIT.with(|hook| *hook.borrow_mut() = Some(Box::new(move || { *slot.lock().unwrap() = Some(journal(&observer)); })));
                Some(captured)
            }
            _ => None,
        };
        work(&mut store);
        lww::BEFORE_HELD_COMMIT.with(|hook| hook.borrow_mut().take());
        generation::BEFORE_CHECKPOINT_COMMIT.with(|hook| hook.borrow_mut().take());
        if let Some(captured) = lost {
            let issued = captured.lock().unwrap().take().expect("the lost segment committed output");
            store.device_store().unwrap().connection().execute("UPDATE device_settings SET value=?1 WHERE key=?2", [issued.as_str(), JOURNAL_KEY]).unwrap();
            return Err(StoreError::Validation { message: "synthetic-lost-reply".into() });
        }
        Ok(())
    }
}

struct Harness { directory: tempfile::TempDir, store: PersistentStore, intent: OllamaIntent, access: Arc<TestAccess>, host: OwnerHost, message_count: usize }

fn setup(origin: &str, root: Value, messages: Option<Value>) -> Harness { setup_with(origin, root, messages, &[]) }

// Further conversations of the same character, each a separate generation target.
fn setup_many(origin: &str, conversations: &[&str]) -> Harness { setup_with(origin, json!({}), None, conversations) }

fn setup_with(origin: &str, root: Value, messages: Option<Value>, conversations: &[&str]) -> Harness {
    let fixture = text_fixture();
    let mut database = fixture["root"].clone();
    database["ollamaURL"] = json!(origin);
    for (key, value) in root.as_object().unwrap() { database[key] = value.clone(); }
    let mut conversation = fixture["conversation"].clone();
    conversation["message"] = messages.unwrap_or_else(|| fixture["messages"].clone());
    let mut character = fixture["character"].clone();
    let mut chats = vec![conversation.clone()];
    chats.extend(conversations.iter().map(|id| { let mut chat = conversation.clone(); chat["id"] = json!(id); chat }));
    character["chats"] = json!(chats);
    database["characters"] = json!([character]);
    database["botPresets"] = json!([fixture["preset"].clone()]);
    setup_database(database)
}

// The first character's first conversation with preset-a is the generation target.
fn setup_database(mut database: Value) -> Harness {
    let characters = database.as_object_mut().unwrap().remove("characters").unwrap();
    let message_count = characters[0]["chats"][0]["message"].as_array().unwrap().len();
    let presets = database.as_object_mut().unwrap().remove("botPresets").unwrap();
    let directory = tempfile::tempdir().unwrap();
    let mut store = PersistentStore::open(directory.path()).unwrap();
    let stage = store.replace_begin().unwrap();
    store.replace_put_root(&stage.staging_id, &database).unwrap();
    store.replace_put_presets(&stage.staging_id, presets.as_array().unwrap()).unwrap();
    store.replace_add_characters(&stage.staging_id, characters.as_array().unwrap()).unwrap();
    store.replace_commit(&stage.staging_id, Some(0)).unwrap();
    let intent = OllamaIntent { effective_preset_id: "preset-a".into(), host: None, target: Target {
        character_id: "character".into(), conversation_id: "conversation".into(), expected_revision: store.revision().unwrap(),
        library_identity: store.library_identity().unwrap(), binding_authority: store.lww_binding_authority().unwrap() } };
    let access = Arc::new(TestAccess { store: Mutex::new(store.open_native_job_store().unwrap()), faults: Mutex::new(VecDeque::new()) });
    Harness { directory, store, intent, access, host: OwnerHost::new(USER_AGENT).unwrap(), message_count }
}

impl Harness {
    fn reserve(&mut self) -> Ticket { self.reserve_in("conversation") }
    fn reserve_in(&mut self, conversation_id: &str) -> Ticket {
        self.intent.target.conversation_id = conversation_id.into();
        self.intent.target.expected_revision = self.store.revision().unwrap();
        self.intent.target.binding_authority = self.store.lww_binding_authority().unwrap();
        service::reserve_ollama(&mut self.store, &self.intent).unwrap()
    }
    fn ready(&mut self) -> ReadyOllama { self.ready_in("conversation", RegistryLimits::PRODUCTION) }
    fn ready_in(&mut self, conversation_id: &str, limits: RegistryLimits) -> ReadyOllama {
        let ticket = self.reserve_in(conversation_id);
        match service::prepare_ready_ollama_limited(&mut self.store, &ticket, &self.host, limits).unwrap() {
            ReadyPreparation::Ready(ready) => { assert!(ready.status().phase == Phase::Running && ready.status().preparation_ready); ready }
            other => panic!("not ready: {other:?}"),
        }
    }
    fn faults(&self, faults: Vec<Option<Fault>>) { *self.access.faults.lock().unwrap() = faults.into(); }
    // Output tapes name dispatch then output; Variables and the fenced context read precede those segments.
    fn output_faults(&self, mut faults: Vec<Option<Fault>>) { faults.insert(1, None); faults.insert(0, None); self.faults(faults); }
    fn execute_with(&self, ready: ReadyOllama, limits: OwnerLimits) -> tokio::task::JoinHandle<OwnerExecution> {
        tokio::spawn(owner::execute_ready_ollama(ready, self.access.clone(), limits))
    }
    fn execute(&self, ready: ReadyOllama) -> tokio::task::JoinHandle<OwnerExecution> { self.execute_with(ready, OwnerLimits::production()) }
    fn conversation(&self) -> Value { self.store.read_conversation("character", "conversation", None).unwrap().unwrap().value }
    fn messages(&self) -> Vec<Value> { self.conversation()["message"].as_array().unwrap().clone() }
    fn evidence(&self, ticket: &Ticket) -> OwnerStatus { service::owner_status(&self.store, ticket).unwrap() }
    fn commit(&mut self, commit: WorkingSetCommit) { self.store.commit(&WorkingSetCommit { expected_revision: self.store.revision().unwrap(), ..commit }).unwrap(); }
}

// A guard against a hung owner, not a timing assertion.
async fn finish(task: tokio::task::JoinHandle<OwnerExecution>) -> OwnerExecution {
    tokio::time::timeout(std::time::Duration::from_secs(60), task).await.expect("owner execution did not finish").unwrap()
}

fn phase(execution: &OwnerExecution) -> Phase { execution.result.as_ref().unwrap().phase.clone() }

fn failure(category: &str, detail: Option<&str>, stage: Option<&str>) -> Option<FailureDescriptor> {
    Some(FailureDescriptor { category: category.into(), detail: detail.map(Into::into), stage: stage.map(Into::into), http_status: None })
}

fn now_millis() -> u64 {
    u64::try_from(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis()).unwrap()
}

// TS counts the send and stamps the interaction time once, then records the memory marker before the request.
fn assert_send_effects(harness: &Harness, since: u64) {
    assert_eq!(harness.store.read_root(None).unwrap().value["statics"], json!({"messages": 1, "imports": 0}));
    let interaction = harness.store.read_character("character", None).unwrap().unwrap().value["lastInteraction"].as_u64().unwrap();
    assert!(interaction >= since && interaction <= now_millis());
    assert_eq!(harness.conversation()["lastMemory"], "NewChat");
}

fn assert_output_row(harness: &Harness, ticket: &Ticket, execution: &OwnerExecution, data: &str) {
    let messages = harness.messages();
    assert_eq!(messages.len(), harness.message_count + 1);
    let row = messages.last().unwrap();
    let status = harness.store.generation_status(ticket).unwrap();
    assert_eq!((row["role"].as_str(), row["data"].as_str(), row["saying"].as_str()), (Some("char"), Some(data), Some("character")));
    assert_eq!(row["chatId"].as_str(), Some(status.message_id.as_str()));
    let info = &row["generationInfo"];
    assert_eq!((info["model"].as_str(), info["generationId"].as_str()), (Some("Ollama Local synthetic-model"), Some(status.message_id.as_str())));
    assert!(["inputTokens", "outputTokens", "maxContext"].iter().all(|key| info[key].is_u64()));
    let timing = info["stageTiming"].as_object().unwrap();
    assert_eq!(timing.keys().map(String::as_str).collect::<Vec<_>>(), ["stage1", "stage2", "stage3", "stage4"]);
    assert!(timing.values().all(Value::is_u64));
    assert_eq!(row["promptInfo"], json!({}));
    let receipt = execution.output.clone().expect("exact output receipt");
    assert_eq!((receipt.source_sequence, receipt.output_hash.as_str()), (1, risunest_sync_wire::hash(data.as_bytes()).as_str()));
    assert_eq!(status.committed_revision, Some(receipt.receipt_revision));
    assert_eq!(harness.evidence(ticket).output, OutputState::Committed(receipt));
}

#[tokio::test]
async fn succ_buffered_literals_store_the_trimmed_row_under_its_exact_receipt() {
    let cases: Vec<_> = response_cases().into_iter().filter(|case| case.route == "success" && !case.id.starts_with("output-")).collect();
    assert_eq!(cases.iter().map(|case| case.id.as_str()).collect::<Vec<_>>(),
        ["text-trim", "text-empty", "text-pair", "text-high-unit", "text-low-unit", "thinking-coerces-number", "text-cbs"]);
    for case in cases {
        let server = serve(vec![reply(case.content.get(), case.thinking.as_ref().map(|value| value.get()))]).await;
        let mut harness = setup(&server.origin, json!({}), None);
        let ready = harness.ready();
        let ticket = ready.ticket().clone();
        #[cfg(not(target_os = "windows"))]
        let execution = finish(harness.execute(ready)).await;
        #[cfg(target_os = "windows")]
        let execution = {
            use crate::ollama_ui_verification::{adapters, controller::{Controller, Gate, Kind, ResultKind}};
            let controller = Controller::default();
            let token = controller.arm(ticket.clone(), Gate::None).unwrap();
            let observation = controller.session(&token).unwrap();
            let actor = observation.enroll();
            let access = harness.access.clone();
            let execution = finish(tokio::spawn(async move {
                adapters::owner_scope(actor, owner::execute_ready_ollama(ready, access, OwnerLimits::production())).await
            })).await;
            let trace = observation.snapshot();
            assert!(trace.drained);
            assert_eq!(trace.invalid, None);
            assert_eq!(trace.events.iter().map(|event| event.kind).collect::<Vec<_>>(), [
                Kind::RequestFormatted, Kind::BufferDecoded, Kind::JsonParsed, Kind::ResponseTransformed,
                Kind::CreditEntered, Kind::CreditDelegated, Kind::CreditForwarded,
                Kind::OfferEntered, Kind::OfferDelegated, Kind::StoragePolicyApplied,
                Kind::OutputSegmentEntered, Kind::OutputAdmitted, Kind::OutputResolved,
                Kind::OfferForwarded, Kind::OwnerCompleted,
            ]);
            assert_eq!(trace.events.iter().find(|event| event.kind == Kind::OutputResolved).unwrap().result, Some(ResultKind::Committed));
            assert_eq!(trace.events.iter().find(|event| event.kind == Kind::OfferForwarded).unwrap().result, Some(ResultKind::Accepted));
            execution
        };
        assert_eq!(phase(&execution), Phase::Completed, "{}", case.id);
        // Policy T: isolated units are stored as U+FFFD, paired units keep their scalar.
        let expected = case.native_output.clone().unwrap_or_else(|| serde_json::from_str(case.output.get()).unwrap());
        assert_output_row(&harness, &ticket, &execution, &expected);
        let evidence = harness.evidence(&ticket);
        assert_eq!((evidence.dispatch, evidence.cancel_decision, evidence.failure), (DispatchState::Issued, None, None), "{}", case.id);
        assert_eq!(server.requests().len(), 1);
        assert_eq!(service::resolve_owner(&mut harness.store, &ticket).unwrap().phase, Phase::Completed);
    }
}

#[tokio::test]
async fn wire_request_matches_the_typescript_oracle_and_ignores_settings_changed_after_ready() {
    let server = serve(vec![reply(r#""Synthetic response.""#, None)]).await;
    let mut harness = setup(&server.origin, json!({}), None);
    let ready = harness.ready();
    assert_eq!(ready.config().model_label(), "synthetic-model");
    let ticket = ready.ticket().clone();
    let changed = [("ollamaURL", json!("http://127.0.0.1:9")), ("ollamaModel", json!("changed-model")), ("ollamaThinkingMode", json!("on")), ("useStreaming", json!(true))];
    harness.commit(WorkingSetCommit { root_mutations: Some(changed.into_iter().map(|(key, value)| RootMutation::Set { key: key.into(), value }).collect()), ..Default::default() });
    let execution = finish(harness.execute(ready)).await;
    assert_eq!(phase(&execution), Phase::Completed);
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, "/api/chat");
    assert_eq!(requests[0].header("user-agent"), Some(USER_AGENT));
    assert_eq!(std::str::from_utf8(&requests[0].body).unwrap(), ORACLE_REQUEST);
    assert_output_row(&harness, &ticket, &execution, "Synthetic response.");
}

#[tokio::test]
async fn perr_failures_keep_only_the_policy_a_fallback_row() {
    let mut cases: Vec<_> = response_cases().into_iter().filter(|case| case.route != "success")
        .map(|case| (case.id.clone(), reply(case.content.get(), None), Some(case))).collect();
    cases.push(("http-500".into(), Reply::Respond { status: "500 Internal Server Error", body: br#"{"error":"Synthetic failure"}"#.to_vec() }, None));
    for (id, response, case) in cases {
        let server = serve(vec![response]).await;
        let mut harness = setup(&server.origin, json!({}), None);
        let since = now_millis();
        let ready = harness.ready();
        let ticket = ready.ticket().clone();
        let execution = finish(harness.execute(ready)).await;
        assert_eq!(phase(&execution), Phase::Failed, "{id}");
        let evidence = harness.evidence(&ticket);
        let expected = match id.as_str() {
            "array-coercion" => failure("type", Some("trim"), Some("output")),
            "array-shadowed-tostring" => failure("type", Some("object-to-primitive"), Some("fallback")),
            "array-marker" => failure("type", Some("substring"), Some("transform")),
            "number-without-thinking" => failure("type", Some("indexOf"), Some("transform")),
            "http-500" => Some(FailureDescriptor { http_status: Some(500), ..failure("http", None, None).unwrap() }),
            other => panic!("unexpected case {other}"),
        };
        assert_eq!(evidence.failure, expected, "{id}");
        assert_eq!(execution.diagnostic.as_ref().map(|value| Some(value.failure.clone())), Some(expected), "{id}");
        if id == "http-500" {
            assert_eq!(execution.diagnostic.as_ref().unwrap().message.as_deref(), Some("Synthetic failure".encode_utf16().collect::<Vec<_>>().as_slice()));
        }
        match case.filter(|case| case.route == "fallback-error") {
            // Ordinary append writes the coerced fallback, then the original failure stands.
            Some(case) => {
                assert_output_row(&harness, &ticket, &execution, &serde_json::from_str::<String>(case.output.get()).unwrap());
                let row = harness.messages().pop().unwrap();
                assert_eq!(row["generationInfo"]["stageTiming"]["stage3"], 0);
                assert_eq!(row["generationInfo"]["stageTiming"]["stage4"], 0);
            },
            None => {
                assert_eq!((harness.messages().len(), execution.output.as_ref(), evidence.output), (3, None, OutputState::None), "{id}");
                assert_eq!(harness.store.generation_status(&ticket).unwrap().checkpoint.0, 0);
            }
        }
        assert_eq!(server.requests().len(), 1);
        assert_send_effects(&harness, since);
        assert_eq!(service::resolve_owner(&mut harness.store, &ticket).unwrap().phase, Phase::Failed);
        assert_send_effects(&harness, since);
    }
}

#[tokio::test]
async fn unsupported_output_fails_before_any_checkpoint() {
    // Writes remain for the current-chat pass; the joined fallback text of an array is guarded too.
    for (content, stage) in [(r#""{{setvar::mood::bright}}bright""#, "output"), (r#"["Hello {{", "char}}."]"#, "fallback")] {
        let server = serve(vec![reply(content, None)]).await;
        let mut harness = setup(&server.origin, json!({}), None);
        let ready = harness.ready();
        let ticket = ready.ticket().clone();
        let execution = finish(harness.execute(ready)).await;
        assert_eq!(phase(&execution), Phase::Failed, "{stage}");
        assert_eq!(harness.evidence(&ticket).failure, failure("unsupported-output", None, Some(stage)));
        assert_eq!(harness.store.generation_status(&ticket).unwrap().checkpoint.0, 0);
        assert_eq!(harness.messages().len(), 3);
    }
}

#[tokio::test]
async fn canb_cancel_or_preflight_failure_before_the_request_leaves_sends_nothing() {
    for cancellation in ["before", "held", "signal-only"] {
        let held_claim = cancellation == "held";
        let server = serve(vec![reply(r#""unused""#, None)]).await;
        let mut harness = setup(&server.origin, json!({}), None);
        let since = now_millis();
        let ready = harness.ready();
        let ticket = ready.ticket().clone();
        let execution = if held_claim {
            // The cancel is recorded before the dispatch claim is admitted.
            let (entered, reached) = oneshot::channel();
            let (release, released) = mpsc::channel();
            harness.faults(vec![Some(Fault::Hold { entered, release: released })]);
            let task = harness.execute(ready);
            reached.await.unwrap();
            assert_eq!(service::cancel_generation(&mut harness.store, &ticket).unwrap().phase, Phase::Running);
            release.send(()).unwrap();
            finish(task).await
        } else {
            if cancellation == "signal-only" {
                assert!(matches!(owner::signal(&owner::scope(&harness.store).unwrap(), &ticket, CancelReason::User), owner::Presence::Live));
            } else {
                assert_eq!(service::cancel_generation(&mut harness.store, &ticket).unwrap().phase, Phase::Running);
            }
            finish(harness.execute(ready)).await
        };
        assert_eq!(phase(&execution), Phase::Cancelled);
        let evidence = harness.evidence(&ticket);
        assert_eq!((evidence.dispatch, evidence.cancel_decision), (DispatchState::None, (cancellation != "signal-only").then_some(CancelReason::User)));
        assert!(execution.dispatch_id.is_none());
        assert_eq!((server.requests().len(), harness.messages().len()), (0, 3));
        if cancellation == "signal-only" { assert_send_effects(&harness, since); }
        else {
            assert_eq!(harness.store.read_root(None).unwrap().value["statics"], json!({"messages":1,"imports":0}));
            assert!(harness.store.read_character("character", None).unwrap().unwrap().value["lastInteraction"].as_u64().unwrap() >= since);
            assert!(harness.conversation().get("lastMemory").is_none());
        }
    }
    let server = serve(vec![reply(r#""unused""#, None)]).await;
    let mut harness = setup(&server.origin, json!({}), None);
    let ready = harness.ready();
    let ticket = ready.ticket().clone();
    let production = OwnerLimits::production();
    let invalid = OwnerLimits { transport: super::ollama_transport::TransportLimits { max_request_bytes: 0, ..production.transport }, ..production };
    let execution = finish(harness.execute_with(ready, invalid)).await;
    assert_eq!(phase(&execution), Phase::Failed);
    let evidence = harness.evidence(&ticket);
    assert_eq!((evidence.dispatch, evidence.failure), (DispatchState::None, failure("limit", Some("invalid-transport-limits"), Some("preflight"))));
    assert_eq!(server.requests().len(), 0);
    assert_eq!(harness.conversation()["lastMemory"], "NewChat");
    // The transport's URL rules run before the claim; userinfo would otherwise reach this server.
    for (scheme, detail) in [("ftp://", "invalid-host"), ("http://user@", "userinfo")] {
        let server = serve(vec![reply(r#""unused""#, None)]).await;
        let host = format!("{scheme}{}", server.origin.trim_start_matches("http://"));
        let mut harness = setup(&host, json!({}), None);
        let ready = harness.ready();
        let ticket = ready.ticket().clone();
        let execution = finish(harness.execute(ready)).await;
        assert_eq!(phase(&execution), Phase::Failed, "{detail}");
        let evidence = harness.evidence(&ticket);
        assert_eq!((evidence.dispatch, evidence.failure), (DispatchState::None, failure("type", Some(detail), Some("preflight"))));
        assert!(execution.dispatch_id.is_none());
        assert_eq!(server.requests().len(), 0);
        assert_eq!(harness.conversation()["lastMemory"], "NewChat");
    }
}

#[derive(Debug, Clone, Copy)]
enum Cana { Headers, Body, BeforeAdmission, AfterAdmission }

#[tokio::test]
async fn cana_cancel_after_the_request_linearizes_at_output_segment_admission() {
    for mode in [Cana::Headers, Cana::Body, Cana::BeforeAdmission, Cana::AfterAdmission] {
        let response = match mode {
            Cana::Headers => Reply::HoldHeaders,
            Cana::Body => Reply::HoldBody(br#"{"message":{"content":"partial"#.to_vec()),
            Cana::BeforeAdmission | Cana::AfterAdmission => reply(r#""Committed before cancel.""#, None),
        };
        let server = serve(vec![response]).await;
        let mut harness = output_harness(&server.origin, &json!({"character":{"customscript":[output_rule("before", "after")]}}));
        let since = now_millis();
        let ready = harness.ready();
        let ticket = ready.ticket().clone();
        let (entered, reached) = oneshot::channel();
        let (release, released) = mpsc::channel();
        match mode {
            Cana::BeforeAdmission => harness.output_faults(vec![None, Some(Fault::Hold { entered, release: released })]),
            Cana::AfterAdmission => harness.output_faults(vec![None, Some(Fault::Admitted(entered))]),
            _ => drop(entered),
        }
        let task = harness.execute(ready);
        match mode {
            Cana::Headers | Cana::Body => server.seen().await,
            _ => reached.await.unwrap(),
        }
        // After admission this waits for the output segment to finish under the ticket gate.
        assert_eq!(service::cancel_generation(&mut harness.store, &ticket).unwrap().phase, Phase::Running, "{mode:?}");
        let _ = release.send(());
        let execution = finish(task).await;
        assert_eq!(phase(&execution), Phase::Cancelled, "{mode:?}");
        let evidence = harness.evidence(&ticket);
        assert_eq!((evidence.dispatch, evidence.cancel_decision), (DispatchState::Issued, Some(CancelReason::User)), "{mode:?}");
        assert_eq!(server.requests().len(), 1);
        if matches!(mode, Cana::AfterAdmission) {
            assert_output_row(&harness, &ticket, &execution, "Committed after cancel.");
        } else {
            assert_eq!((harness.messages().len(), execution.output.is_none(), evidence.output), (3, true, OutputState::None), "{mode:?}");
        }
        assert_send_effects(&harness, since);
        assert_eq!(service::resolve_owner(&mut harness.store, &ticket).unwrap().phase, Phase::Cancelled);
        assert_send_effects(&harness, since);
    }
}

#[tokio::test]
async fn lost_output_reply_after_the_library_commit_resolves_once_without_retry() {
    // Lost reply resolved by the owner; resolution also unavailable; terminal persistence unavailable.
    for faults in [vec![None, Some(Fault::LoseReply)], vec![None, Some(Fault::LoseReply), Some(Fault::Unavailable)],
        vec![None, None, Some(Fault::Unavailable)]] {
        let deferred = faults.len() == 3;
        let server = serve(vec![reply(r#""Committed once.""#, None)]).await;
        let mut harness = output_harness(&server.origin, &json!({"character":{"customscript":[output_rule("once", "ONCE")]}}));
        let ready = harness.ready();
        let ticket = ready.ticket().clone();
        harness.output_faults(faults);
        let execution = finish(harness.execute(ready)).await;
        if deferred {
            assert!(execution.result.is_err());
            assert_eq!(harness.store.generation_status(&ticket).unwrap().phase, Phase::Running);
            // The recovery obligation stays in the registry and settles without another request.
            assert_eq!(service::resolve_owner(&mut harness.store, &ticket).unwrap().phase, Phase::Completed);
        } else {
            assert_eq!(phase(&execution), Phase::Completed);
        }
        assert_eq!(harness.messages().len(), 4);
        let OutputState::Committed(receipt) = harness.evidence(&ticket).output else { panic!("output receipt") };
        assert_eq!(harness.store.generation_status(&ticket).unwrap().committed_revision, Some(receipt.receipt_revision));
        assert_eq!(harness.messages()[3]["data"], "Committed ONCE.");
        assert_eq!(receipt.output_hash, risunest_sync_wire::hash(b"Committed ONCE."));
        assert_eq!(server.requests().len(), 1);
    }
}

#[tokio::test]
async fn dup_duplicate_execute_dropped_ready_and_unpolled_execute_issue_at_most_one_request() {
    let release = Arc::new(Notify::new());
    let server = serve(vec![gated(&release, reply(r#""Only once.""#, None))]).await;
    let mut harness = setup(&server.origin, json!({}), None);
    let ready = harness.ready();
    let ticket = ready.ticket().clone();
    let task = harness.execute(ready);
    server.seen().await;
    assert!(matches!(service::prepare_ready_ollama(&mut harness.store, &ticket, &harness.host).unwrap(),
        ReadyPreparation::Existing(status) if status.phase == Phase::Running));
    release.notify_one();
    assert_eq!(phase(&finish(task).await), Phase::Completed);
    assert!(matches!(service::prepare_ready_ollama(&mut harness.store, &ticket, &harness.host).unwrap(),
        ReadyPreparation::Existing(status) if status.phase == Phase::Completed));
    assert_eq!(server.requests().len(), 1);

    let server = serve(Vec::new()).await;
    let messages = text_fixture()["effectMessages"].clone();
    let mut harness = setup(&server.origin, json!({}), Some(messages));
    for unpolled in [false, true] {
        let ready = harness.ready();
        let ticket = ready.ticket().clone();
        let normalized = harness.messages();
        assert!(normalized.iter().all(|row| !row["chatId"].is_null()), "ID effects precede Ready");
        if unpolled { drop(owner::execute_ready_ollama(ready, harness.access.clone(), OwnerLimits::production())); } else { drop(ready); }
        assert_eq!(service::owner_status(&harness.store, &ticket).unwrap().status.phase, Phase::Running);
        // The next reservation's bounded pass settles the abandoned owner, or an explicit resolve does.
        let status = if unpolled { harness.reserve(); harness.store.generation_status(&ticket).unwrap() }
            else { service::resolve_owner(&mut harness.store, &ticket).unwrap() };
        assert_eq!(status.phase, Phase::Cancelled);
        assert_eq!(harness.evidence(&ticket).dispatch, DispatchState::None);
        assert_eq!(harness.messages(), normalized);
    }
    assert_eq!(server.requests().len(), 0);
}

const RESTART_TEST: &str = "RISUNEST_NATIVE_OWNER_RESTART_TEST";
const RESTART_ROOT: &str = "RISUNEST_NATIVE_OWNER_RESTART_ROOT";
const RESTART_TICKET: &str = "RISUNEST_NATIVE_OWNER_RESTART_TICKET";

#[tokio::test]
async fn dup_restart_after_issued_interrupts_without_another_request() {
    let test = "dup_restart_after_issued_interrupts_without_another_request";
    if std::env::var(RESTART_TEST).ok().as_deref() == Some(test) {
        let root = std::path::PathBuf::from(std::env::var_os(RESTART_ROOT).expect("child store path"));
        let ticket: Ticket = serde_json::from_str(&std::env::var(RESTART_TICKET).unwrap()).unwrap();
        let mut store = PersistentStore::open(&root).unwrap();
        assert_eq!(service::resolve_owner(&mut store, &ticket).unwrap().phase, Phase::Interrupted);
        return;
    }
    let server = serve(vec![Reply::HoldHeaders]).await;
    let mut harness = setup(&server.origin, json!({}), None);
    let ready = harness.ready();
    let ticket = ready.ticket().clone();
    let task = harness.execute(ready);
    server.seen().await;
    // The process loses its owner after the request left; dropping it performs no store work.
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    let module = module_path!().split_once("::").unwrap().1;
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", &format!("{module}::{test}"), "--nocapture"])
        .env(RESTART_TEST, test).env(RESTART_ROOT, harness.directory.path()).env(RESTART_TICKET, serde_json::to_string(&ticket).unwrap())
        .status().expect("start isolated Rust test process");
    assert!(status.success(), "restarted process failed: {status}");
    let evidence = harness.evidence(&ticket);
    assert_eq!((evidence.status.phase.clone(), evidence.dispatch), (Phase::Interrupted, DispatchState::Uncertain));
    assert_eq!(service::resolve_owner(&mut harness.store, &ticket).unwrap().phase, Phase::Interrupted);
    assert_eq!((server.requests().len(), harness.messages().len()), (1, 3));
}

#[tokio::test]
async fn edit_foreign_target_message_change_ends_stale_and_unrelated_variable_edit_survives() {
    for message_edit in [true, false] {
        let release = Arc::new(Notify::new());
        let server = serve(vec![gated(&release, reply(r#""Reply after edits.""#, None))]).await;
        let mut harness = output_harness(&server.origin, &json!({"character":{"customscript":[output_rule("after", "AFTER")]}}));
        let ready = harness.ready();
        let ticket = ready.ticket().clone();
        let task = harness.execute(ready);
        server.seen().await;
        let variables = json!({"newer": "variable"});
        let mut commit = WorkingSetCommit { unit_mutations: Some(vec![lww::UnitMutation::Set {
            key: UnitKey::new(&["conversation", "character", "conversation", "scriptstate"]).unwrap(), value: variables.clone() }]), ..Default::default() };
        if message_edit {
            let mut message = harness.messages()[0].clone();
            message["data"] = json!("Edited during the wait.");
            commit.conversations = Some(vec![ConversationMutation::ReplaceRange { character_id: "character".into(), conversation_id: "conversation".into(),
                start: 0, delete_count: 1, messages: vec![message], conversation: None, configured_index: None }]);
        }
        harness.commit(commit);
        release.notify_one();
        let execution = finish(task).await;
        assert_eq!(harness.conversation()["scriptstate"], variables);
        if message_edit {
            assert_eq!((phase(&execution), execution.diagnostic.is_none()), (Phase::Stale, true));
            assert_eq!((harness.messages().len(), harness.messages()[0]["data"].as_str()), (3, Some("Edited during the wait.")));
            assert_eq!(harness.evidence(&ticket).output, OutputState::None);
        } else {
            assert_eq!(phase(&execution), Phase::Completed);
            assert_output_row(&harness, &ticket, &execution, "Reply AFTER edits.");
        }
        assert_eq!(server.requests().len(), 1);
    }
}

#[tokio::test]
async fn binding_change_during_the_operation_ends_stale() {
    let release = Arc::new(Notify::new());
    let server = serve(vec![gated(&release, reply(r#""Reply after binding change.""#, None))]).await;
    let mut harness = setup(&server.origin, json!({}), None);
    let ready = harness.ready();
    let ticket = ready.ticket().clone();
    let task = harness.execute(ready);
    server.seen().await;
    let selection = harness.store.external_selection().unwrap();
    harness.store.lww_switch_target(&lww::Header { binding_authority: harness.store.lww_binding_authority().unwrap(), request_id: uuid::Uuid::new_v4().to_string() },
        &BindingSelectionChange { expected_epoch: selection.epoch, new_epoch: uuid::Uuid::new_v4().to_string(), target: SyncTarget::None,
            library_id: None, target_id: None, inspection_id: None, initial_publication: false }).unwrap();
    release.notify_one();
    let execution = finish(task).await;
    assert_eq!((phase(&execution), execution.diagnostic.is_none()), (Phase::Stale, true));
    assert_eq!((harness.messages().len(), execution.output.is_none()), (3, true));
    assert_eq!(harness.evidence(&ticket).output, OutputState::None);
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn sync_binding_and_effective_streaming_are_refused_before_dispatch() {
    let server = serve(Vec::new()).await;
    let mut harness = setup(&server.origin, json!({}), None);
    for target in [SyncTarget::Server("synthetic-server".into()), SyncTarget::External("synthetic-external".into())] {
        let selection = harness.store.external_selection().unwrap();
        harness.store.external_select(&selection.epoch, &target).unwrap();
        assert!(matches!(service::reserve_ollama(&mut harness.store, &harness.intent),
            Err(GenerationError::Preparation(PreparationError::Unsupported(field))) if field == "syncBinding"));
    }
    let selection = harness.store.external_selection().unwrap();
    harness.store.external_select(&selection.epoch, &SyncTarget::None).unwrap();
    let streaming = |value: bool| WorkingSetCommit { root_mutations: Some(vec![RootMutation::Set { key: "useStreaming".into(), value: json!(value) }]), ..Default::default() };
    harness.commit(streaming(true));
    harness.intent.target.expected_revision = harness.store.revision().unwrap();
    assert!(matches!(service::reserve_ollama(&mut harness.store, &harness.intent),
        Err(GenerationError::Preparation(PreparationError::Unsupported(field))) if field == "useStreaming"));
    // A change after reservation is refused by the Ready capture from the same snapshot.
    harness.commit(streaming(false));
    let ticket = harness.reserve();
    harness.commit(streaming(true));
    assert!(matches!(service::prepare_ready_ollama(&mut harness.store, &ticket, &harness.host).unwrap(),
        ReadyPreparation::Failed { error: PreparationError::Unsupported(field), status } if field == "useStreaming" && status.phase == Phase::Failed));
    assert_eq!(server.requests().len(), 0);
}

fn is_code<T>(result: &StoreResult<T>, code: &str) -> bool {
    matches!(result, Err(StoreError::Validation { message }) if message == &format!("native-generation:{code}"))
}

fn edit_first_message(store: &mut PersistentStore, data: &str) {
    let conversation = store.read_conversation("character", "conversation", None).unwrap().unwrap().value;
    let mut message = conversation["message"][0].clone();
    message["data"] = json!(data);
    store.commit(&WorkingSetCommit { expected_revision: store.revision().unwrap(), conversations: Some(vec![ConversationMutation::ReplaceRange {
        character_id: "character".into(), conversation_id: "conversation".into(), start: 0, delete_count: 1, messages: vec![message],
        conversation: None, configured_index: None }]), ..Default::default() }).unwrap();
}

#[tokio::test]
async fn a_target_edit_racing_the_output_commit_ends_stale_without_a_failure_decision() {
    let server = serve(vec![reply(r#""Raced reply.""#, None)]).await;
    let mut harness = setup(&server.origin, json!({}), None);
    let ready = harness.ready();
    let ticket = ready.ticket().clone();
    let mut racer = harness.store.open_native_job_store().unwrap();
    harness.output_faults(vec![None, Some(Fault::Race(Box::new(move || edit_first_message(&mut racer, "Edited in the race."))))]);
    let execution = finish(harness.execute(ready)).await;
    assert_eq!((phase(&execution), execution.output.is_none(), execution.diagnostic.is_none()), (Phase::Stale, true, true));
    let evidence = harness.evidence(&ticket);
    assert_eq!((evidence.output, evidence.failure), (OutputState::None, None));
    assert_eq!((harness.messages().len(), harness.messages()[0]["data"].as_str()), (3, Some("Edited in the race.")));
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn an_owner_lost_after_dispatch_is_interrupted_unless_a_caller_cancelled() {
    for cancelled in [false, true] {
        let server = serve(vec![Reply::HoldHeaders]).await;
        let mut harness = setup(&server.origin, json!({}), None);
        let ready = harness.ready();
        let ticket = ready.ticket().clone();
        let task = harness.execute(ready);
        server.seen().await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        // Presence::Abandoned: the caller's signal settles through recovery of the dropped owner.
        let status = if cancelled { service::cancel_generation(&mut harness.store, &ticket).unwrap() }
            else { service::resolve_owner(&mut harness.store, &ticket).unwrap() };
        let evidence = harness.evidence(&ticket);
        if cancelled {
            assert_eq!((status.phase, evidence.dispatch, evidence.cancel_decision), (Phase::Cancelled, DispatchState::Issued, Some(CancelReason::User)));
        } else {
            assert_eq!((status.phase, evidence.dispatch, evidence.cancel_decision), (Phase::Interrupted, DispatchState::Uncertain, None));
        }
        assert_eq!((server.requests().len(), harness.messages().len()), (1, 3));
    }
    // Before any dispatch, a dropped capability cancelled by its caller keeps that reason.
    let server = serve(Vec::new()).await;
    let mut harness = setup(&server.origin, json!({}), None);
    let ready = harness.ready();
    let ticket = ready.ticket().clone();
    drop(ready);
    assert_eq!(service::cancel_generation(&mut harness.store, &ticket).unwrap().phase, Phase::Cancelled);
    let evidence = harness.evidence(&ticket);
    assert_eq!((evidence.dispatch, evidence.cancel_decision), (DispatchState::None, Some(CancelReason::User)));
    assert_eq!(server.requests().len(), 0);
}

#[tokio::test]
async fn settle_lost_applies_the_foreign_recovery_rule_only_for_the_record_owner() {
    let server = serve(Vec::new()).await;
    let mut harness = setup(&server.origin, json!({}), None);
    let ticket = harness.reserve();
    let Start::Started { owner, .. } = harness.store.generation_resume_preparation(&ticket).unwrap() else { panic!("not started") };
    harness.store.generation_ensure_history_ids(&owner).unwrap();
    let lifecycle = uuid::Uuid::new_v4().to_string();
    harness.store.generation_mark_ready_owned(&owner, &lifecycle).unwrap();
    harness.store.generation_send_variables(&owner, &lifecycle, &super::preparation_effects::PreparedVariablePlan::default()).unwrap();
    harness.store.generation_send_memory(&owner, &lifecycle, None).unwrap();
    assert!(matches!(harness.store.generation_claim_dispatch(&owner, &lifecycle).unwrap(), DispatchClaim::Issued { .. }));
    let before = journal(&harness.store);
    let foreign = Owner { token: uuid::Uuid::new_v4().to_string(), ..owner.clone() };
    assert!(is_code(&harness.store.generation_settle_lost(&foreign), "stale-owner"));
    assert_eq!(journal(&harness.store), before);
    assert_eq!(harness.store.generation_settle_lost(&owner).unwrap().phase, Phase::Interrupted);
    assert_eq!(harness.evidence(&ticket).dispatch, DispatchState::Uncertain);
    // The first terminal phase stands.
    assert_eq!(harness.store.generation_settle_lost(&owner).unwrap().phase, Phase::Interrupted);
    assert_eq!(harness.messages().len(), 3);
}

#[tokio::test]
async fn a_full_registry_settles_abandoned_owners_first_and_otherwise_refuses_without_writing() {
    let server = serve(Vec::new()).await;
    let mut harness = setup_many(&server.origin, &["conversation-2", "conversation-3"]);
    // No bounded pass, so only the full pass at the cap can make room.
    let limits = RegistryLimits { capacity: 2, pass: 0 };
    let first = harness.ready_in("conversation", limits);
    let second = harness.ready_in("conversation-2", limits);
    let ticket = harness.reserve_in("conversation-3");
    let before = journal(&harness.store);
    assert!(is_code(&service::prepare_ready_ollama_limited(&mut harness.store, &ticket, &harness.host, limits), "lifecycle-limit"));
    assert_eq!(journal(&harness.store), before);
    assert_eq!(harness.store.generation_status(&ticket).unwrap().phase, Phase::Reserved);
    let abandoned = first.ticket().clone();
    drop(first);
    let third = match service::prepare_ready_ollama_limited(&mut harness.store, &ticket, &harness.host, limits).unwrap() {
        ReadyPreparation::Ready(ready) => ready,
        other => panic!("not ready: {other:?}"),
    };
    assert_eq!(harness.store.generation_status(&abandoned).unwrap().phase, Phase::Cancelled);
    let tickets = [second.ticket().clone(), third.ticket().clone()];
    drop((second, third));
    owner::reconcile(&mut harness.store).unwrap();
    assert!(tickets.iter().all(|ticket| harness.store.generation_status(ticket).unwrap().phase == Phase::Cancelled));
}

#[tokio::test]
async fn reconcile_moves_entries_that_keep_failing_behind_the_others() {
    let server = serve(Vec::new()).await;
    let mut harness = setup_many(&server.origin, &["conversation-2", "conversation-3"]);
    let mut readies = Vec::new();
    for conversation in ["conversation", "conversation-2", "conversation-3"] { readies.push(harness.ready_in(conversation, RegistryLimits::PRODUCTION)); }
    let tickets: Vec<Ticket> = readies.iter().map(|ready| ready.ticket().clone()).collect();
    drop(readies);
    // The two oldest entries keep failing, as an unresolved offer would.
    let failing = tickets[..2].to_vec();
    let mut resolve = |store: &mut PersistentStore, ticket: &Ticket| {
        if failing.contains(ticket) { Err(StoreError::CommitBusy) } else { owner::resolve_owner(store, ticket) }
    };
    for _ in 0..2 { owner::reconcile_limited(&mut harness.store, 2, &mut resolve).unwrap(); }
    let phases: Vec<Phase> = tickets.iter().map(|ticket| harness.store.generation_status(ticket).unwrap().phase).collect();
    assert_eq!(phases, [Phase::Running, Phase::Running, Phase::Cancelled]);
    owner::reconcile(&mut harness.store).unwrap();
    assert!(tickets.iter().all(|ticket| harness.store.generation_status(ticket).unwrap().phase == Phase::Cancelled));
}

#[tokio::test]
async fn an_abandoned_entry_on_a_stale_target_resolves_its_offer_and_ends_stale() {
    let server = serve(vec![reply(r#""Committed before the switch.""#, None)]).await;
    let mut harness = setup(&server.origin, json!({}), None);
    let ready = harness.ready();
    let ticket = ready.ticket().clone();
    harness.output_faults(vec![None, Some(Fault::LoseReply), Some(Fault::Unavailable)]);
    assert!(finish(harness.execute(ready)).await.result.is_err());
    let scope = owner::scope(&harness.store).unwrap();
    assert!(owner::registered(&scope, &ticket));
    let selection = harness.store.external_selection().unwrap();
    harness.store.lww_switch_target(&lww::Header { binding_authority: harness.store.lww_binding_authority().unwrap(), request_id: uuid::Uuid::new_v4().to_string() },
        &BindingSelectionChange { expected_epoch: selection.epoch, new_epoch: uuid::Uuid::new_v4().to_string(), target: SyncTarget::None,
            library_id: None, target_id: None, inspection_id: None, initial_publication: false }).unwrap();
    // The offer's receipt outlives the switch: it resolves once, and the record ends Stale and leaves the registry.
    let status = service::resolve_owner(&mut harness.store, &ticket).unwrap();
    assert_eq!((status.phase, status.pending_commit), (Phase::Stale, false));
    assert!(!owner::registered(&scope, &ticket));
    let OutputState::Committed(receipt) = harness.evidence(&ticket).output else { panic!("output receipt") };
    assert_eq!(status.committed_revision, Some(receipt.receipt_revision));
    assert_eq!(harness.messages().len(), 4);
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn active_generations_list_reserved_and_running_records_without_writing() {
    let server = serve(vec![reply(r#""Done.""#, None)]).await;
    let mut harness = setup_many(&server.origin, &["conversation-2", "conversation-3"]);
    let done = harness.ready();
    assert_eq!(phase(&finish(harness.execute(done)).await), Phase::Completed);
    let running = harness.ready_in("conversation-2", RegistryLimits::PRODUCTION);
    let reserved = harness.reserve_in("conversation-3");
    let before = journal(&harness.store);
    let active = service::active_generations(&harness.store).unwrap();
    let entry = |ticket: &Ticket, conversation: &str, phase: Phase| ActiveGeneration {
        ticket: ticket.clone(), library_identity: harness.store.library_identity().unwrap(), character_id: "character".into(), conversation_id: conversation.into(), phase };
    assert_eq!(active, [entry(running.ticket(), "conversation-2", Phase::Running), entry(&reserved, "conversation-3", Phase::Reserved)]);
    assert_eq!(serde_json::to_value(&active[1]).unwrap(),
        json!({"ticket": serde_json::to_value(&reserved).unwrap(), "libraryIdentity": harness.store.library_identity().unwrap(), "characterId": "character", "conversationId": "conversation-3", "phase": "reserved"}));
    assert_eq!(journal(&harness.store), before);
}

fn switch_binding(store: &mut PersistentStore) {
    let selection = store.external_selection().unwrap();
    store.lww_switch_target(&lww::Header { binding_authority: store.lww_binding_authority().unwrap(), request_id: uuid::Uuid::new_v4().to_string() },
        &BindingSelectionChange { expected_epoch: selection.epoch, new_epoch: uuid::Uuid::new_v4().to_string(), target: SyncTarget::None,
            library_id: None, target_id: None, inspection_id: None, initial_publication: false }).unwrap();
}

#[tokio::test]
async fn lost_memory_effect_reply_resolves_once_and_sends_nothing() {
    let server = serve(vec![reply(r#""unused""#, None)]).await;
    let mut harness = setup(&server.origin, json!({}), None);
    let since = now_millis();
    let ready = harness.ready();
    let ticket = ready.ticket().clone();
    let revision = harness.store.revision().unwrap();
    // The claim segment commits the memory marker, then loses the journal's resolution and the reply.
    harness.faults(vec![None, Some(Fault::LoseReply)]);
    let execution = finish(harness.execute(ready)).await;
    assert_eq!(phase(&execution), Phase::Failed);
    let evidence = harness.evidence(&ticket);
    assert_eq!((evidence.dispatch, evidence.failure), (DispatchState::None, failure("interrupted", Some("dispatch-claim"), Some("dispatch"))));
    assert!(!evidence.status.pending_commit);
    assert_eq!((harness.store.revision().unwrap(), evidence.status.committed_revision), (revision + 1, Some(revision + 1)));
    assert_send_effects(&harness, since);
    assert_eq!(service::resolve_owner(&mut harness.store, &ticket).unwrap().phase, Phase::Failed);
    assert_eq!(harness.store.revision().unwrap(), revision + 1);
    assert_eq!((server.requests().len(), harness.messages().len()), (0, 3));
}

// Holds device admission on another connection, as a renderer command's commit does, until `release` fires.
fn hold_admission(directory: &std::path::Path) -> (mpsc::Sender<()>, std::thread::JoinHandle<()>) {
    let renderer = PersistentStore::open(directory).unwrap();
    let (held, holding) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    let holder = std::thread::spawn(move || renderer.holding_admission(|| { held.send(()).unwrap(); let _ = released.recv(); }).unwrap());
    holding.recv().unwrap();
    (release, holder)
}

fn admission_wait(deadline: std::time::Duration) -> lww::AdmissionWait {
    lww::AdmissionWait { deadline, first_pause: std::time::Duration::from_millis(5), max_pause: std::time::Duration::from_millis(20) }
}

#[tokio::test]
async fn the_dispatch_claim_waits_within_its_bound_for_device_admission_a_renderer_command_holds() {
    for bounded in [false, true] {
        let server = serve(vec![reply(r#""Done.""#, None)]).await;
        let mut harness = setup(&server.origin, json!({}), None);
        let ready = harness.ready();
        let (ticket, revision) = (ready.ticket().clone(), harness.store.revision().unwrap());
        let deadline = std::time::Duration::from_millis(if bounded { 150 } else { 10_000 });
        harness.access.store.lock().unwrap().set_admission_wait(admission_wait(deadline));
        let (entered, reached) = oneshot::channel();
        let (resume, resumed) = mpsc::channel();
        // Past the bound, the segment that settles the failed claim waits here until admission is free again.
        if bounded { harness.faults(vec![None, None, Some(Fault::Hold { entered, release: resumed })]); }
        let (release, holder) = hold_admission(harness.directory.path());
        let task = harness.execute(ready);
        if bounded { reached.await.unwrap(); } else { tokio::time::sleep(std::time::Duration::from_millis(100)).await; }
        // The claim's memory marker commit is still waiting, so nothing was committed or sent.
        assert_eq!((harness.store.revision().unwrap(), server.requests().len()), (revision, 0));
        release.send(()).unwrap();
        holder.join().unwrap();
        let _ = resume.send(());
        let execution = finish(task).await;
        let evidence = harness.evidence(&ticket);
        if bounded {
            assert_eq!(phase(&execution), Phase::Failed);
            assert_eq!((evidence.dispatch, evidence.failure), (DispatchState::None, failure("interrupted", Some("dispatch-claim"), Some("dispatch"))));
            assert_eq!((server.requests().len(), harness.messages().len()), (0, 3));
        } else {
            assert_eq!(phase(&execution), Phase::Completed);
            assert_eq!((server.requests().len(), harness.messages().len()), (1, 4));
        }
    }
}

#[tokio::test]
async fn prompt_info_inside_chat_is_refused_before_any_send_effect() {
    let server = serve(Vec::new()).await;
    let mut harness = setup(&server.origin, json!({"promptInfoInsideChat": true}), None);
    let (revision, root) = (harness.store.revision().unwrap(), harness.store.read_root(None).unwrap().value);
    assert!(matches!(service::reserve_ollama(&mut harness.store, &harness.intent),
        Err(GenerationError::Preparation(PreparationError::Unsupported(field))) if field == "promptInfoInsideChat"));
    assert_eq!((harness.store.revision().unwrap(), harness.store.read_root(None).unwrap().value), (revision, root));
    // Enabled after reservation, it is refused by the admission that precedes the interaction effect.
    let setting = |value: bool| WorkingSetCommit { root_mutations: Some(vec![RootMutation::Set { key: "promptInfoInsideChat".into(), value: json!(value) }]), ..Default::default() };
    harness.commit(setting(false));
    let ticket = harness.reserve();
    harness.commit(setting(true));
    let revision = harness.store.revision().unwrap();
    assert!(matches!(service::prepare_ready_ollama(&mut harness.store, &ticket, &harness.host).unwrap(),
        ReadyPreparation::Failed { error: PreparationError::Unsupported(field), status } if field == "promptInfoInsideChat" && status.phase == Phase::Failed));
    assert_eq!(harness.store.revision().unwrap(), revision);
    assert!(harness.store.read_root(None).unwrap().value.get("statics").is_none());
    assert_eq!(server.requests().len(), 0);
}

#[tokio::test]
async fn a_retargeted_pending_entry_ends_stale_and_frees_its_target_at_the_cap() {
    let server = serve(vec![reply(r#""Committed before the switch.""#, None), reply(r#""Sent after the switch.""#, None)]).await;
    let mut harness = setup(&server.origin, json!({}), None);
    let limits = RegistryLimits { capacity: 1, pass: 0 };
    let ready = harness.ready_in("conversation", limits);
    let ticket = ready.ticket().clone();
    harness.output_faults(vec![None, Some(Fault::LoseReply), Some(Fault::Unavailable)]);
    assert!(finish(harness.execute(ready)).await.result.is_err());
    assert!(harness.store.generation_status(&ticket).unwrap().pending_commit);
    switch_binding(&mut harness.store);
    // The full pass at the cap resolves the entry: its receipt settles, the record ends Stale, and the target is free.
    let next = harness.ready_in("conversation", limits);
    let status = harness.store.generation_status(&ticket).unwrap();
    assert_eq!((status.phase, status.pending_commit), (Phase::Stale, false));
    assert!(!owner::registered(&owner::scope(&harness.store).unwrap(), &ticket));
    let next_ticket = next.ticket().clone();
    let execution = finish(harness.execute(next)).await;
    assert_eq!(phase(&execution), Phase::Completed);
    let messages = harness.messages();
    assert_eq!(messages.len(), 5);
    assert_eq!((messages[3]["data"].as_str(), messages[4]["data"].as_str()), (Some("Committed before the switch."), Some("Sent after the switch.")));
    assert_eq!(messages[4]["chatId"].as_str(), Some(harness.store.generation_status(&next_ticket).unwrap().message_id.as_str()));
    assert_eq!(server.requests().len(), 2);
}

#[tokio::test]
async fn an_unrelated_commit_racing_the_output_commit_retries_and_completes() {
    let server = serve(vec![reply(r#""Raced reply.""#, None)]).await;
    let mut harness = setup(&server.origin, json!({}), None);
    let ready = harness.ready();
    let ticket = ready.ticket().clone();
    let mut racer = harness.store.open_native_job_store().unwrap();
    harness.output_faults(vec![None, Some(Fault::Race(Box::new(move || {
        racer.commit(&WorkingSetCommit { expected_revision: racer.revision().unwrap(),
            root_mutations: Some(vec![RootMutation::Set { key: "language".into(), value: json!("ko") }]), ..Default::default() }).unwrap();
    })))]);
    let execution = finish(harness.execute(ready)).await;
    assert_eq!(phase(&execution), Phase::Completed);
    assert_output_row(&harness, &ticket, &execution, "Raced reply.");
    assert_eq!(harness.store.read_root(None).unwrap().value["language"], "ko");
    assert_eq!(server.requests().len(), 1);
}

// The observation the TS oracle records for the output row; object key order is not part of the stored value.
fn output_observation(row: &Value, from: u64, to: u64) -> Value {
    let sorted = |value: &Value| { let mut keys: Vec<String> = value.as_object().unwrap().keys().cloned().collect(); keys.sort(); keys };
    let info = &row["generationInfo"];
    let timing = &info["stageTiming"];
    json!({
        "keys": sorted(row), "role": row["role"], "saying": row["saying"], "text": row["data"],
        "timeWithinSend": row["time"].as_u64().is_some_and(|time| time >= from && time <= to),
        "chatIdIsGenerationId": row["chatId"].as_str().is_some_and(|id| !id.is_empty() && info["generationId"].as_str() == Some(id)),
        "promptInfo": row["promptInfo"],
        "generationInfo": { "keys": sorted(info), "model": info["model"], "inputTokens": info["inputTokens"],
            "outputTokens": info["outputTokens"], "maxContext": info["maxContext"], "stageTimingKeys": sorted(timing),
            "stageTimingWholeMilliseconds": timing.as_object().unwrap().values().all(Value::is_u64), "stage2": timing["stage2"] },
    })
}

#[tokio::test]
#[ignore = "requires the real TypeScript Ollama preparation oracle output"]
async fn send_effects_and_output_metadata_match_the_typescript_oracle() {
    let path = std::env::var_os("RISUNEST_OLLAMA_PREPARATION_ORACLE").expect("synthetic TS oracle path");
    let oracle: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let fixture = text_fixture();
    assert_eq!(oracle["effects"].as_array().unwrap().len(), fixture["effects"].as_array().unwrap().len());
    for expected in oracle["effects"].as_array().unwrap() {
        let (id, outcome) = (expected["id"].as_str().unwrap(), expected["outcome"].as_str().unwrap());
        let case = fixture["effects"].as_array().unwrap().iter().find(|case| case["id"] == id).unwrap();
        let replies = match outcome {
            "provider-failure" => vec![Reply::Respond { status: match case["status"].as_u64().unwrap_or(500) {
                500 => "500 Internal Server Error", 503 => "503 Service Unavailable", 429 => "429 Too Many Requests", other => panic!("{id}: {other}"),
            }, body: br#"{"error":"Synthetic provider failure."}"#.to_vec() }],
            "abort" => vec![Reply::HoldHeaders],
            // The connection closes without a response, as the fetch TS sees fail.
            "network-failure" => Vec::new(),
            _ => vec![reply(r#""Synthetic effect response.""#, None)],
        };
        let server = serve(replies).await;
        let mut database = expected["database"].clone();
        if outcome != "invalid-host" { database["ollamaURL"] = json!(server.origin); }
        let mut harness = setup_database(database);
        if case["observeLore"] == true { harness.intent.host = Some(output_host(false)); }
        // The route refuses a trigger row before reservation, with the name its `decline` holds.
        if let Some(name) = case["decline"].as_str() {
            let error = service::reserve_ollama(&mut harness.store, &harness.intent).unwrap_err();
            assert!(matches!(&error, GenerationError::Preparation(PreparationError::Unsupported(refused)) if refused == name), "{id}: {error:?}");
            assert!(server.requests().is_empty(), "{id}");
            continue;
        }
        let stored = harness.messages().len();
        let from = now_millis();
        let ticket = harness.reserve();
        match service::prepare_ready_ollama(&mut harness.store, &ticket, &harness.host).unwrap() {
            ReadyPreparation::Ready(ready) => {
                if outcome == "abort-before-request" { assert!(matches!(owner::signal(&owner::scope(&harness.store).unwrap(), &ticket, CancelReason::User), owner::Presence::Live)); }
                let task = harness.execute(ready);
                if outcome == "abort" {
                    server.seen().await;
                    service::cancel_generation(&mut harness.store, &ticket).unwrap();
                }
                let expected_phase = match outcome { "success" => Phase::Completed, "provider-failure" | "network-failure" | "invalid-host" => Phase::Failed, "abort" | "abort-before-request" => Phase::Cancelled, other => panic!("{id}: {other}") };
                let execution = finish(task).await;
                assert_eq!(phase(&execution), expected_phase, "{id}");
                // The failure reports the provider's error text, the message TS rejects the send with.
                if outcome == "provider-failure" {
                    let message = execution.diagnostic.as_ref().and_then(|diagnostic| diagnostic.message.as_deref()).map(String::from_utf16_lossy);
                    assert_eq!(message.as_deref(), Some(expected["error"].as_str().unwrap()), "{id}");
                }
            }
            ReadyPreparation::Failed { error, .. } => assert_eq!((outcome, error), ("context-failure", PreparationError::ContextLimit), "{id}"),
            ReadyPreparation::Existing(status) => panic!("{id}: {status:?}"),
        }
        // One attempt whatever `requestRetrys` holds, as TS makes.
        let request_expected = matches!(outcome, "success" | "provider-failure" | "network-failure" | "abort");
        assert_eq!(server.requests().len() as u64, expected["requests"].as_u64().unwrap(), "{id}");
        assert_eq!(server.requests().len(), usize::from(request_expected), "{id}");
        // The bytes on the wire are the request body the TS send sent.
        let body = server.requests().first().map(|request| String::from_utf8(request.body.clone()).unwrap());
        assert_eq!(body.as_deref(), expected["body"].as_str(), "{id}");
        assert_eq!(harness.evidence(&ticket).dispatch, if request_expected { DispatchState::Issued } else { DispatchState::None }, "{id}");
        let to = now_millis();
        let effects = &expected["sendEffects"];
        assert_eq!(harness.store.read_root(None).unwrap().value["statics"], effects["statics"]["after"], "{id}");
        let interaction = harness.store.read_character("character", None).unwrap().unwrap().value["lastInteraction"].as_u64();
        assert_eq!(Some(interaction.is_some_and(|time| time >= from && time <= to)), effects["lastInteraction"]["afterWithinSend"].as_bool(), "{id}");
        assert_eq!(harness.conversation().get("lastMemory").cloned().unwrap_or(Value::Null), effects["lastMemory"]["after"], "{id}");
        let messages = harness.messages();
        let output = if outcome == "success" { output_observation(messages.last().unwrap(), from, to) } else { Value::Null };
        assert_eq!(messages.len(), stored + usize::from(outcome == "success"), "{id}");
        assert_eq!(output, effects["output"], "{id}");
        // The start trigger's writes, which the owner stores with the request or with the failure.
        if case["observeTriggers"] == true {
            let (conversation, observation) = (harness.conversation(), &expected["triggerObservation"]);
            assert_eq!(conversation["scriptstate"], observation["storedScriptstate"], "{id}");
            assert_eq!(conversation["note"], observation["storedNote"], "{id}");
        }
    }
    // Every other start trigger row the route runs, through the owner: the scriptstate and author note it stores are the
    // ones TS stored. `trigger-random-cbs-order` draws `Math.random()` at the reservation, which this run does not pin;
    // `matches_real_typescript_ollama_oracle` compares it under the recorded draws.
    let recorded: Value = serde_json::from_str(include_str!("../../../tests/fixtures/rust-core/prompt/ollama-text.expected.json")).unwrap();
    let mut compared = 0;
    for case in fixture["cases"].as_array().unwrap() {
        let id = case["id"].as_str().unwrap();
        if case["observeTriggers"] != true || case["decline"].is_string() || id == "trigger-random-cbs-order" { continue; }
        let expected = oracle["cases"].as_array().unwrap().iter().find(|entry| entry["id"] == id).unwrap();
        let content = case.get("responseContent").map_or_else(|| json!("Synthetic trigger response.").to_string(), Value::to_string);
        let server = serve(vec![reply(&content, None)]).await;
        let mut database = expected["database"].clone();
        database["ollamaURL"] = json!(server.origin);
        let mut harness = setup_database(database);
        let host = recorded["cases"].as_array().unwrap().iter().find(|entry| entry["id"] == id).unwrap()["result"].get("host").cloned();
        harness.intent.host = host.map(|host| serde_json::from_value(host).unwrap());
        let original = harness.messages();
        let ready = harness.ready();
        assert_eq!(phase(&finish(harness.execute(ready)).await), Phase::Completed, "{id}");
        let (conversation, observation) = (harness.conversation(), &expected["triggerObservation"]);
        assert_eq!(conversation["scriptstate"], observation["storedScriptstate"], "{id}");
        assert_eq!(conversation["note"], observation["storedNote"], "{id}");
        // The history the start trigger left, with the output row after it.
        let messages = harness.messages();
        crate::persistent_store::generation::preparation_tests::assert_stored_rows(&messages[..messages.len() - 1],
            &observation["storedMessages"], &original, id);
        if case.get("responseContent").is_some() {
            assert_eq!(messages.last().unwrap()["data"], case["expectedOutput"], "{id}");
        }
        compared += 1;
    }
    assert_eq!(compared, 15);
}

// The start trigger's message edits run through the owner: the stored history is the one TS stored (the Send oracle's
// `storedMessages`), the output row follows it, and the edits are part of the send's evidence.
#[tokio::test]
async fn start_trigger_message_edits_store_the_rows_typescript_stored() {
    use crate::persistent_store::generation::preparation_tests::{assert_stored_rows, database, recorded_host};
    let mut impersonated = vec![
        json!({"chatId":"message-a","data":" First question. ","futureMessage":{"preserve":true},"role":"user"}),
        json!({"chatId":"message-b","data":"<Thoughts>Previous hidden thought.</Thoughts> First answer. ","role":"char"}),
        json!({"chatId":"message-c","data":" Second question. ","role":"user"})];
    impersonated.extend([json!({"data":"Impersonated user Synthetic character.","role":"user","chatId":"appended-1"}),
        json!({"data":"Impersonated reply.","role":"char","chatId":"appended-2"})]);
    let pinned = [
        ("trigger-modifychat", json!([{"chatId":"message-a","data":"Edited first question.","futureMessage":{"preserve":true},"role":"user"},
            {"chatId":"message-b","data":"Edited answer Synthetic character.","role":"char"},
            {"role":"user","data":" Second question. ","chatId":"message-c"}])),
        ("trigger-cutchat", json!([{"role":"user","data":"Kept third.","chatId":"cut-c"},{"role":"char","data":"Kept fourth.","chatId":"cut-d"},
            {"role":"user","data":"Kept fifth.","chatId":"cut-e"}])),
        ("trigger-cutchat-allbefore", json!([{"role":"char","data":"Hidden answer.","chatId":"reset-b"},
            {"role":"char","data":"Reset.","chatId":"reset-r","disabled":"allBefore"},{"role":"user","data":"Visible question.","chatId":"reset-c"}])),
        ("trigger-impersonate", Value::Array(impersonated)),
    ];
    let fixture = text_fixture();
    for (id, rows) in pinned {
        let case = fixture["cases"].as_array().unwrap().iter().find(|case| case["id"] == id).unwrap();
        assert_eq!(case["triggerEditsMessages"], true, "{id}");
        let server = serve(vec![reply(&case["responseContent"].to_string(), None)]).await;
        let mut root = database(case);
        root["ollamaURL"] = json!(server.origin);
        let mut harness = setup_database(root);
        harness.intent.host = recorded_host(case);
        let original = harness.messages();
        let ready = harness.ready();
        let ticket = ready.ticket().clone();
        assert_eq!(phase(&finish(harness.execute(ready)).await), Phase::Completed, "{id}");
        let messages = harness.messages();
        assert_stored_rows(&messages[..messages.len() - 1], &rows, &original, id);
        let output = messages.last().unwrap();
        let status = harness.store.generation_status(&ticket).unwrap();
        assert_eq!((&output["data"], output["chatId"].as_str()), (&case["expectedOutput"], Some(status.message_id.as_str())), "{id}");
        let evidence = harness.store.generation_effects_evidence(&ticket).unwrap().unwrap();
        assert!(evidence.affected_unit_keys.iter().any(|key| key == r#"["messages","character","conversation"]"#), "{id}");
        assert_eq!(server.requests().len(), 1, "{id}");
    }
}


#[tokio::test]
async fn abandoned_quarantined_offer_reports_stale_without_losing_pending_evidence() {
    for uncertain in [false, true] {
        let server = serve(vec![reply(r#""Synthetic uncertain output.""#, None)]).await;
        let mut harness = setup(&server.origin, json!({}), None);
        let ready = harness.ready();
        let ticket = ready.ticket().clone();
        harness.output_faults(vec![None, Some(Fault::InterruptedOffer), Some(Fault::Unavailable)]);
        assert!(finish(harness.execute(ready)).await.result.is_err());
        if uncertain {
            let stage = harness.store.replace_begin().unwrap();
            harness.store.replace_put_root(&stage.staging_id, &json!({"syntheticReplacement":true})).unwrap();
            harness.store.replace_put_presets(&stage.staging_id, &[]).unwrap();
            harness.store.replace_commit(&stage.staging_id, Some(harness.store.revision().unwrap())).unwrap();
        } else {
            switch_binding(&mut harness.store);
        }
        let status = service::resolve_owner(&mut harness.store, &ticket).unwrap();
        assert_eq!((status.phase.clone(), status.pending_commit), (Phase::Stale, uncertain));
        assert_eq!(service::resolve_owner(&mut harness.store, &ticket).unwrap(), status);
        assert_eq!(service::active_generations(&harness.store).unwrap().iter().any(|entry| entry.ticket == ticket), uncertain);
        if uncertain { assert_eq!(harness.store.read_root(None).unwrap().value["syntheticReplacement"], true); }
        else { assert_eq!(harness.messages().len(), 3); }
        assert_eq!(server.requests().len(), 1);
    }
}

fn output_rule(pattern: &str, replacement: &str) -> Value {
    json!({"type":"editoutput", "in":pattern, "out":replacement})
}

fn output_host(excluded: bool) -> super::cbs::HostCapture {
    use super::cbs::{CaseUnicode, HostCapture, RegexUnicode};
    let now = now_millis() as i64;
    HostCapture::new(false, CaseUnicode::Unicode16, vec![(now - 86_400_000, 0)], now + 86_400_000)
        .with_regex(RegexUnicode::Unicode17, excluded)
}

fn output_harness(origin: &str, case: &Value) -> Harness {
    let fixture = text_fixture();
    let patched = |field: &str| {
        let mut value = fixture[field].clone();
        if let Some(patch) = case[field].as_object() { value.as_object_mut().unwrap().extend(patch.iter().map(|(key, value)| (key.clone(), value.clone()))); }
        value
    };
    let mut database = patched("root");
    let mut character = patched("character");
    let mut conversation = patched("conversation");
    conversation["message"] = case.get("messages").unwrap_or(&fixture["messages"]).clone();
    character["chats"] = json!([conversation]);
    database["characters"] = json!([character]);
    database["botPresets"] = json!([patched("preset")]);
    database["ollamaURL"] = json!(origin);
    let mut harness = setup_database(database);
    harness.intent.host = Some(output_host(case["startupExclusions"].as_array().is_some_and(|values| values.iter().any(|value| value == "regex"))));
    harness
}

#[tokio::test]
async fn editoutput_scripts_store_the_real_typescript_fixture_text_and_receipt() {
    let cases: Vec<Value> = raw_response_cases().into_iter().filter_map(|raw| {
        let definition: ResponseCase = serde_json::from_str(raw.get()).unwrap();
        (definition.oracle_scope == "owner-output").then(|| serde_json::from_str(raw.get()).unwrap())
    }).filter(|case: &Value| case.get("nativeUnsupported").is_none() && case.get("midflightVariables").is_none()).collect();
    assert_eq!(cases.len(), 7);
    for case in &cases {
        let server = serve(vec![reply(&case["content"].to_string(), None)]).await;
        let mut harness = output_harness(&server.origin, case);
        let mut ready = harness.ready();
        // The response phase starts after the distinct prompt draw. A reset to the preparation tape produces A X.
        if case["id"] == "output-random" {
            assert_eq!(case["random"], json!([0.125, 0.875, 0.375]));
            ready.pin_response_entropy(super::cbs::Entropy::pinned(now_millis() as f64, &[0.875, 0.375]));
        }
        let ticket = ready.ticket().clone();
        let execution = finish(harness.execute(ready)).await;
        assert_eq!(phase(&execution), Phase::Completed, "{}: {:?}", case["id"], execution.diagnostic);
        assert_output_row(&harness, &ticket, &execution, case["output"].as_str().unwrap());
        assert_eq!(server.requests().len(), 1, "{}", case["id"]);
    }
}

// The response revalidation refuses the triggers prompt admission refuses, including those TS runs in every mode.
#[test]
fn response_admission_refuses_triggers_led_by_triggercode_or_triggerlua() {
    for first in ["triggercode", "triggerlua"] {
        let mut harness = output_harness("http://127.0.0.1:9", &json!({"character":{"triggerscript":[
            {"comment":"","type":"manual","conditions":[],"effect":[{"type":first,"code":"synthetic"}]}]}}));
        let snapshot = harness.store.generation_preparation_snapshot(&harness.intent).unwrap();
        let refused = super::prepare::admit_response(&snapshot).map(|_| ());
        assert!(matches!(refused, Err(risunest_ir::stage::StageError::Unsupported(ref name)) if name == "triggers"), "{first}: {refused:?}");
    }
}

#[tokio::test]
async fn editoutput_refusals_are_before_dispatch_or_terminal_after_response_assembly() {
    for replacement in ["{{screenwidth}}", "{{getvar::unsafe}}", "{{setvar::mood::bright}}bright"] {
        let server = serve(vec![reply(r#""seed""#, None)]).await;
        let mut harness = output_harness(&server.origin, &json!({"character":{"customscript":[output_rule("absent", replacement)]},
            "conversation":{"scriptstate":{"$unsafe":"{{screenwidth}}"}}}));
        let revision = harness.store.revision().unwrap();
        let before = harness.conversation();
        let result = service::reserve_ollama(&mut harness.store, &harness.intent);
        assert!(matches!(result, Err(GenerationError::Preparation(PreparationError::Unsupported(ref name))) if name == "regex.replacementCbs"), "{result:?}");
        assert_eq!(harness.store.revision().unwrap(), revision);
        assert_eq!(harness.conversation(), before);
        assert!(service::active_generations(&harness.store).unwrap().is_empty());
        assert!(server.requests().is_empty());
    }
    // Only the response supplies the remainder of this CBS write. The configured replacement is a literal brace.
    let server = serve(vec![reply(r#""{setvar::mood::bright}}bright""#, None)]).await;
    let mut harness = output_harness(&server.origin, &json!({"character":{"customscript":[output_rule("^", "{")]}}));
    let before = harness.conversation()["scriptstate"].clone();
    let ready = harness.ready();
    let ticket = ready.ticket().clone();
    let execution = finish(harness.execute(ready)).await;
    assert_eq!(phase(&execution), Phase::Failed);
    assert_eq!(harness.evidence(&ticket).failure, failure("unsupported-output", None, Some("output")));
    assert_eq!(harness.messages().len(), harness.message_count);
    assert_eq!(harness.conversation()["scriptstate"], before);
    assert_eq!(harness.evidence(&ticket).output, OutputState::None);
    assert_eq!(server.requests().len(), 1);
    assert_eq!(service::resolve_owner(&mut harness.store, &ticket).unwrap().phase, Phase::Failed);
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn editoutput_limits_bound_the_transformed_bytes_and_nontext_keeps_policy_a() {
    for (replacement, completed) in [("éé", true), ("ééé", false)] {
        let server = serve(vec![reply(r#""x""#, None)]).await;
        let mut harness = output_harness(&server.origin, &json!({"character":{"customscript":[output_rule("x", replacement)]}}));
        let ready = harness.ready();
        let ticket = ready.ticket().clone();
        let mut limits = OwnerLimits::production();
        limits.transport.max_output_bytes = 5;
        let execution = finish(harness.execute_with(ready, limits)).await;
        if completed {
            assert_eq!(phase(&execution), Phase::Completed);
            assert_output_row(&harness, &ticket, &execution, replacement);
        } else {
            assert_eq!(phase(&execution), Phase::Failed);
            assert_eq!(harness.evidence(&ticket).failure, failure("limit", Some("output-bytes"), Some("output")));
            assert_eq!(harness.messages().len(), harness.message_count);
            assert!(execution.output.is_none());
        }
        assert_eq!(server.requests().len(), 1);
    }
    let server = serve(vec![reply(r#"[" alpha ",null,7,["nested","tail"]]"#, None)]).await;
    let mut harness = output_harness(&server.origin, &json!({"character":{"customscript":[output_rule("alpha", "SCRIPT RAN")]}}));
    let ready = harness.ready();
    let ticket = ready.ticket().clone();
    let execution = finish(harness.execute(ready)).await;
    assert_eq!(phase(&execution), Phase::Failed);
    assert_eq!(harness.evidence(&ticket).failure, failure("type", Some("trim"), Some("output")));
    assert_output_row(&harness, &ticket, &execution, "alpha ,,7,nested,tail");
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
#[ignore = "requires the real TypeScript owner response oracle output"]
async fn editoutput_owner_matches_fresh_typescript_response_oracle() {
    use sha2::{Digest, Sha256};
    type RawObject = std::collections::BTreeMap<String, Box<RawValue>>;
    let path = std::env::var_os("RISUNEST_OLLAMA_OWNER_RESPONSE_ORACLE").expect("synthetic TS owner response oracle path");
    let oracle: RawObject = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert!(serde_json::from_str::<bool>(oracle["complete"].get()).unwrap());
    let hashes: std::collections::BTreeMap<String, String> = serde_json::from_str(oracle["sourceHashes"].get()).unwrap();
    for (name, source) in [
        ("tests/fixtures/native-generation/ollama-owner-response.json", include_bytes!("../../../tests/fixtures/native-generation/ollama-owner-response.json").as_slice()),
        ("tests/fixtures/native-generation/ollama-text.json", include_bytes!("../../../tests/fixtures/native-generation/ollama-text.json").as_slice()),
        ("benchmarks/generation-send/ollama-owner-response-oracle.test.ts", include_bytes!("../../../benchmarks/generation-send/ollama-owner-response-oracle.test.ts").as_slice()),
    ] { assert_eq!(hashes[name], hex::encode(Sha256::digest(source)), "{name}"); }
    let definitions: Vec<ResponseCase> = raw_response_cases().into_iter().map(|raw| serde_json::from_str(raw.get()).unwrap()).collect();
    let scoped: Vec<_> = definitions.iter().filter(|case| case.oracle_scope == "owner-output").map(|case| case.id.as_str()).collect();
    assert_eq!(scoped.len(), 13);
    let recorded: Vec<Box<RawValue>> = serde_json::from_str(oracle["cases"].get()).unwrap();
    assert_eq!(recorded.len(), definitions.len());
    let mut ids = Vec::new();
    let mut compared = 0;
    for raw in recorded {
        let fields: RawObject = serde_json::from_str(raw.get()).unwrap();
        let id: String = serde_json::from_str(fields["id"].get()).unwrap();
        ids.push(id.clone());
        if !scoped.contains(&id.as_str()) { continue; }
        let expected: Value = serde_json::from_str(raw.get()).unwrap();
        let case = response_case(&id);
        let observed = &expected["observations"];
        let release = Arc::new(Notify::new());
        let server = serve(vec![gated(&release, reply(case.content.get(), None))]).await;
        let mut database = observed["database"].clone();
        database["ollamaURL"] = json!(server.origin);
        let mut harness = setup_database(database);
        harness.intent.host = Some(output_host(case.id == "output-excluded"));
        let before = harness.conversation();
        if matches!(case.native_unsupported.as_deref(), Some("regex.dynamicPattern" | "regex.replacementCbs")) {
            let revision = harness.store.revision().unwrap();
            assert!(matches!(service::reserve_ollama(&mut harness.store, &harness.intent),
                Err(GenerationError::Preparation(PreparationError::Unsupported(ref name))) if Some(name) == case.native_unsupported.as_ref()), "{}", case.id);
            assert_eq!(harness.store.revision().unwrap(), revision);
            assert_eq!(harness.conversation(), before);
            assert!(server.requests().is_empty());
        } else {
            let mut ready = harness.ready();
            let draws: Vec<_> = observed["random"]["responseDraws"].as_array().unwrap().iter().map(|draw| draw.as_f64().unwrap()).collect();
            ready.pin_response_entropy(super::cbs::Entropy::pinned(now_millis() as f64, &draws));
            let ticket = ready.ticket().clone();
            let task = harness.execute(ready);
            server.seen().await;
            if observed["midflight"].is_object() {
                let database = &observed["midflight"]["database"];
                let variables = database["characters"][0]["chats"][0]["scriptstate"].clone();
                let defaults = case.midflight_defaults.as_ref().map(|_| json!({
                    "characterDefaultVariables":database["characters"][0]["defaultVariables"],
                    "presetTemplateDefaultVariables":database["botPresets"][0]["templateDefaultVariables"]
                }));
                commit_output_context_edit(&mut harness, variables, defaults.as_ref());
            }
            release.notify_one();
            let execution = finish(task).await;
            let persisted = observed["afterExplicitFlush"]["messages"].as_array().unwrap().last().unwrap()["data"].as_str().unwrap();
            let early = expected["events"].as_array().unwrap().iter().find(|event| event["stage"] == "process-output:return").unwrap()["value"]["data"].as_str().unwrap();
            if case.native_unsupported.is_some() {
                assert_ne!(early, persisted, "{}", case.id);
                assert_eq!(phase(&execution), Phase::Failed);
                assert_eq!(harness.messages().len(), harness.message_count);
                assert_eq!(harness.conversation()["scriptstate"], before["scriptstate"]);
                assert!(execution.output.is_none());
            } else {
                assert_eq!(early, persisted, "{}", case.id);
                assert_eq!(phase(&execution), Phase::Completed, "{}: {:?}", case.id, execution.diagnostic);
                assert_output_row(&harness, &ticket, &execution, persisted);
            }
            assert_eq!(server.requests().len(), 1, "{}", case.id);
        }
        compared += 1;
    }
    assert_eq!(ids, definitions.iter().map(|case| case.id.clone()).collect::<Vec<_>>());
    assert_eq!(compared, 13);
}

#[tokio::test]
async fn editoutput_draws_are_fresh_and_unicode_named_captures_keep_their_values() {
    // The preparation records no draws. Replaying that empty tape for the response must fail this real owner send.
    let server = serve(vec![reply(r#""{{random}} {{random}}""#, None)]).await;
    let mut harness = output_harness(&server.origin, &json!({"character":{"customscript":[output_rule("$", " {{random}} ")]}}));
    let ready = harness.ready();
    let ticket = ready.ticket().clone();
    let execution = finish(harness.execute(ready)).await;
    assert_eq!(phase(&execution), Phase::Completed, "{:?}", execution.diagnostic);
    let data = harness.messages().last().unwrap()["data"].as_str().unwrap().to_owned();
    let draws: Vec<f64> = data.split_whitespace().map(|value| value.parse::<f64>().unwrap()).collect();
    assert_eq!(draws.len(), 3);
    assert!(draws.iter().all(|draw| (0.0..1.0).contains(draw)));
    assert!(data.ends_with(' '), "script whitespace is not trimmed again");
    assert_output_row(&harness, &ticket, &execution, &data);
    assert_eq!(server.requests().len(), 1);

    let server = serve(vec![reply(r#""aéz""#, None)]).await;
    let mut harness = output_harness(&server.origin, &json!({"character":{"customscript":[
        {"type":"editoutput","in":"(?<word>é)","out":"[$<word>]","flag":"gu","ableFlag":true},
        {"type":"editoutput","in":"\\p{Ll}","out":"U","flag":"gu","ableFlag":true}
    ]}}));
    let ready = harness.ready();
    let ticket = ready.ticket().clone();
    let execution = finish(harness.execute(ready)).await;
    assert_eq!(phase(&execution), Phase::Completed);
    assert_output_row(&harness, &ticket, &execution, "U[U]U");
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn editoutput_reads_response_time_variables_and_rechecks_new_unsafe_values() {
    for (value, completed) in [("new", true), ("{{screenwidth}}", false)] {
        let release = Arc::new(Notify::new());
        let server = serve(vec![gated(&release, reply(r#""seed""#, None))]).await;
        let mut harness = output_harness(&server.origin, &json!({"conversation":{"scriptstate":{"$mood":"old","preserve":"value"}},
            "character":{"customscript":[output_rule("seed", "{{getvar::mood}}")]}}));
        let ready = harness.ready();
        let ticket = ready.ticket().clone();
        let task = harness.execute(ready);
        server.seen().await;
        let variables = json!({"$mood":value,"preserve":"value"});
        harness.commit(WorkingSetCommit { unit_mutations: Some(vec![lww::UnitMutation::Set {
            key: UnitKey::new(&["conversation", "character", "conversation", "scriptstate"]).unwrap(), value: variables.clone() }]), ..Default::default() });
        release.notify_one();
        let execution = finish(task).await;
        assert_eq!(harness.conversation()["scriptstate"], variables);
        if completed {
            assert_eq!(phase(&execution), Phase::Completed, "{:?}", execution.diagnostic);
            assert_output_row(&harness, &ticket, &execution, "new");
        } else {
            assert_eq!(phase(&execution), Phase::Failed);
            assert_eq!(harness.evidence(&ticket).failure, failure("unsupported-output", Some("regex.replacementCbs"), Some("output")));
            assert_eq!(harness.messages().len(), harness.message_count);
            assert!(execution.output.is_none());
        }
        assert_eq!(server.requests().len(), 1);
    }
}

#[tokio::test]
async fn editoutput_revalidates_new_unsafe_unmatched_rules_before_append() {
    let release = Arc::new(Notify::new());
    let server = serve(vec![gated(&release, reply(r#""seed""#, None))]).await;
    let mut harness = output_harness(&server.origin, &json!({"preset":{"regex":[output_rule("absent", "safe")]},
        "conversation":{"scriptstate":{"$mood":"old"}}}));
    let ready = harness.ready();
    let ticket = ready.ticket().clone();
    let task = harness.execute(ready);
    server.seen().await;
    let mut preset = harness.store.read_preset("preset-a", None).unwrap().unwrap().value;
    preset["regex"] = json!([output_rule("absent", "{{setvar::mood::new}}")]);
    harness.commit(WorkingSetCommit { replace_presets: Some(vec![preset]), ..Default::default() });
    release.notify_one();
    let execution = finish(task).await;
    assert_eq!(phase(&execution), Phase::Failed);
    assert_eq!(harness.evidence(&ticket).failure, failure("unsupported-output", Some("regex.replacementCbs"), Some("output")));
    assert_eq!(harness.messages().len(), harness.message_count);
    assert_eq!(harness.conversation()["scriptstate"]["$mood"], "old");
    assert!(execution.output.is_none());
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn editoutput_rejects_a_module_activated_by_the_captured_preset_during_http() {
    for change in ["activate", "selected-config", "unselected-config"] {
        let release = Arc::new(Notify::new());
        let server = serve(vec![gated(&release, reply(r#""seed""#, None))]).await;
        let selected = change != "activate";
        let mut harness = output_harness(&server.origin, &json!({
            "root":{"modules":[{"id":"late-module","regex":[output_rule("seed", "module result")],
                "lorebook":[{"id":"late-module","key":"","alwaysActive":true,"content":"@@keep_activate_after_match\nMODULE_ACTIVE","insertorder":100}]},
                {"id":"unselected","name":"Unselected"}],"enabledModules":if selected {json!(["late-module"])}else{json!([])}},
            "preset":{"moduleIntergration":""},"character":{"loreSettings":{"tokenBudget":1000,"scanDepth":10,"recursiveScanning":false},
                "customscript":[output_rule("seed", "character result")]}}));
        let ready = harness.ready();
        let ticket = ready.ticket().clone();
        let task = harness.execute(ready);
        server.seen().await;
        assert_eq!(harness.conversation()["scriptstate"].get("$__internal_ka_late-module").is_some(), selected);
        let effects = harness.store.generation_effects_evidence(&ticket).unwrap().unwrap();
        if change == "activate" {
            let mut preset = harness.store.read_preset("preset-a", None).unwrap().unwrap().value;
            preset["moduleIntergration"] = json!("late-module");
            harness.commit(WorkingSetCommit { replace_presets: Some(vec![preset]), ..Default::default() });
        } else {
            let mut modules = harness.store.read_root(None).unwrap().value["modules"].clone();
            if change == "selected-config" { modules[0]["regex"][0]["out"] = json!("changed module result"); }
            else { modules[1]["name"] = json!("Changed unrelated module"); }
            harness.commit(WorkingSetCommit { root_mutations: Some(vec![RootMutation::Set { key:"modules".into(), value:modules }]), ..Default::default() });
        }
        release.notify_one();
        let execution = finish(task).await;
        assert_eq!(phase(&execution), Phase::Failed, "{change}");
        assert_eq!(harness.evidence(&ticket).failure, failure("unsupported-output", Some("modules-changed"), Some("output")));
        assert_eq!(harness.messages().len(), harness.message_count);
        assert!(execution.output.is_none());
        assert_eq!(harness.store.generation_effects_evidence(&ticket).unwrap().unwrap(), effects);
        assert_eq!(server.requests().len(), 1);
    }
}
#[tokio::test]
async fn editoutput_does_not_retarget_a_preset_switched_during_http() {
    let release = Arc::new(Notify::new());
    let server = serve(vec![gated(&release, reply(r#""seed""#, None))]).await;
    let mut harness = output_harness(&server.origin, &json!({"preset":{"regex":[output_rule("seed", "original")]}}));
    let original = harness.store.read_preset("preset-a", None).unwrap().unwrap().value;
    let mut other = original.clone();
    other["id"] = json!("preset-b");
    other["regex"] = json!([output_rule("seed", "other")]);
    harness.commit(WorkingSetCommit { replace_presets: Some(vec![original, other]), ..Default::default() });
    let ready = harness.ready();
    let ticket = ready.ticket().clone();
    let task = harness.execute(ready);
    server.seen().await;
    harness.commit(WorkingSetCommit { root_mutations: Some(vec![RootMutation::Set { key: "botPresetsId".into(), value: json!("preset-b") }]), ..Default::default() });
    release.notify_one();
    let execution = finish(task).await;
    assert_eq!(phase(&execution), Phase::Failed);
    assert_eq!(harness.evidence(&ticket).failure, failure("unsupported-output", Some("preset-selection-changed"), Some("output")));
    assert_eq!(harness.messages().len(), harness.message_count);
    assert!(execution.output.is_none());
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn editoutput_keeps_equivalent_gui_selection_and_reads_same_preset_edits() {
    for (before, after) in [(None, json!(0)), (Some(json!(0)), json!("preset-a")),
        (Some(json!("preset-a")), json!(0)), (Some(json!("missing")), json!("preset-a"))] {
        let release = Arc::new(Notify::new());
        let server = serve(vec![gated(&release, reply(r#""seed""#, None))]).await;
        let mut harness = output_harness(&server.origin, &json!({"preset":{"regex":[output_rule("seed", "old")]}}));
        let mutation = before.map_or_else(|| RootMutation::Delete { key: "botPresetsId".into() },
            |value| RootMutation::Set { key: "botPresetsId".into(), value });
        harness.commit(WorkingSetCommit { root_mutations: Some(vec![mutation]), ..Default::default() });
        let ready = harness.ready();
        let ticket = ready.ticket().clone();
        let task = harness.execute(ready);
        server.seen().await;
        let mut preset = harness.store.read_preset("preset-a", None).unwrap().unwrap().value;
        preset["regex"] = json!([output_rule("seed", "new")]);
        harness.commit(WorkingSetCommit { root_mutations: Some(vec![RootMutation::Set { key: "botPresetsId".into(), value: after }]),
            replace_presets: Some(vec![preset]), ..Default::default() });
        release.notify_one();
        let execution = finish(task).await;
        assert_eq!(phase(&execution), Phase::Completed, "{:?}", execution.diagnostic);
        assert_output_row(&harness, &ticket, &execution, "new");
        assert_eq!(server.requests().len(), 1);
    }
}

#[tokio::test]
async fn editoutput_retains_a_captured_override_independent_of_gui_selection() {
    let release = Arc::new(Notify::new());
    let server = serve(vec![gated(&release, reply(r#""seed""#, None))]).await;
    let mut harness = output_harness(&server.origin, &json!({"preset":{"regex":[output_rule("seed", "gui")]}}));
    let original = harness.store.read_preset("preset-a", None).unwrap().unwrap().value;
    let mut captured = original.clone();
    captured["id"] = json!("preset-b");
    captured["regex"] = json!([output_rule("seed", "captured")]);
    harness.commit(WorkingSetCommit { replace_presets: Some(vec![original.clone(), captured.clone()]), ..Default::default() });
    harness.intent.effective_preset_id = "preset-b".into();
    let ready = harness.ready();
    let ticket = ready.ticket().clone();
    let task = harness.execute(ready);
    server.seen().await;
    captured["regex"] = json!([output_rule("seed", "updated override")]);
    harness.commit(WorkingSetCommit { root_mutations: Some(vec![RootMutation::Set { key: "botPresetsId".into(), value: json!("preset-a") }]),
        replace_presets: Some(vec![original, captured]), ..Default::default() });
    release.notify_one();
    let execution = finish(task).await;
    assert_eq!(phase(&execution), Phase::Completed, "{:?}", execution.diagnostic);
    assert_output_row(&harness, &ticket, &execution, "updated override");
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn editoutput_detects_numeric_gui_selection_changed_by_catalog_reorder() {
    let release = Arc::new(Notify::new());
    let server = serve(vec![gated(&release, reply(r#""seed""#, None))]).await;
    let mut harness = output_harness(&server.origin, &json!({"preset":{"regex":[output_rule("seed", "original")]}}));
    let original = harness.store.read_preset("preset-a", None).unwrap().unwrap().value;
    let mut other = original.clone();
    other["id"] = json!("preset-b");
    other["regex"] = json!([output_rule("seed", "other")]);
    harness.commit(WorkingSetCommit { replace_presets: Some(vec![original.clone(), other.clone()]), ..Default::default() });
    let ready = harness.ready();
    let ticket = ready.ticket().clone();
    let task = harness.execute(ready);
    server.seen().await;
    harness.commit(WorkingSetCommit { replace_presets: Some(vec![other, original]), ..Default::default() });
    release.notify_one();
    let execution = finish(task).await;
    assert_eq!(phase(&execution), Phase::Failed);
    assert_eq!(harness.evidence(&ticket).failure, failure("unsupported-output", Some("preset-selection-changed"), Some("output")));
    assert_eq!(harness.messages().len(), harness.message_count);
    assert!(execution.output.is_none());
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn editoutput_context_read_failure_is_terminal_without_an_output_offer() {
    let server = serve(vec![reply(r#""seed""#, None)]).await;
    let mut harness = output_harness(&server.origin, &json!({"character":{"customscript":[output_rule("seed", "result")]}}));
    let ready = harness.ready();
    let ticket = ready.ticket().clone();
    // Dispatch succeeds, then the new response-time read fails before any transformation or output admission.
    harness.faults(vec![None, None, Some(Fault::Unavailable)]);
    let execution = finish(harness.execute(ready)).await;
    assert_eq!(phase(&execution), Phase::Failed);
    assert_eq!(harness.evidence(&ticket).failure, failure("store", Some("response-context"), Some("output")));
    assert_eq!(harness.messages().len(), harness.message_count);
    assert_eq!(harness.evidence(&ticket).output, OutputState::None);
    assert!(execution.output.is_none());
    assert_eq!(server.requests().len(), 1);
}

fn ready_with_variables(harness: &mut Harness) -> ReadyOllama {
    harness.commit(WorkingSetCommit { unit_mutations: Some(vec![lww::UnitMutation::Set {
        key: UnitKey::new(&["character", "character", "globalLore"]).unwrap(),
        value: json!([{"id":"flag","key":"","alwaysActive":true,"content":"@@keep_activate_after_match\nOwned lore.","insertorder":100}]),
    }, lww::UnitMutation::Set {
        key: UnitKey::new(&["character", "character", "loreSettings"]).unwrap(),
        value: json!({"tokenBudget":1000,"scanDepth":10,"recursiveScanning":false}),
    }]), ..Default::default() });
    harness.intent.host = Some(output_host(false));
    harness.ready()
}

#[tokio::test]
async fn prepared_variables_precede_memory_http_and_feed_live_output_context() {
    for later_edit in [false, true] {
        let release = Arc::new(Notify::new());
        let server = serve(vec![gated(&release, reply(r#""seed""#, None))]).await;
        let mut harness = output_harness(&server.origin, &json!({"character":{"customscript":[output_rule("seed", "{{getvar::__internal_ka_flag}}()")]}}));
        let ready = ready_with_variables(&mut harness);
        let (ticket, revision) = (ready.ticket().clone(), harness.store.revision().unwrap());
        assert!(harness.conversation()["scriptstate"].get("$__internal_ka_flag").is_none());
        let task = harness.execute(ready);
        server.seen().await;
        assert_eq!(harness.conversation()["scriptstate"]["$__internal_ka_flag"], "true");
        assert_eq!(harness.conversation()["lastMemory"], "NewChat");
        let request: Value = serde_json::from_slice(&server.requests()[0].body).unwrap();
        assert!(request["messages"].as_array().unwrap().iter().any(|message| message["content"].as_str().is_some_and(|text| text.contains("Owned lore."))));
        let evidence = harness.store.generation_effects_evidence(&ticket).unwrap().unwrap();
        assert_eq!(evidence.receipts.len(), 3);
        assert_eq!((evidence.receipts[1].revision, evidence.receipts[2].revision), (revision + 1, revision + 2));
        assert_eq!(evidence.affected_unit_keys[2], r#"["conversation","character","conversation","scriptstate"]"#);
        if later_edit {
            harness.commit(WorkingSetCommit { unit_mutations: Some(vec![lww::UnitMutation::Set {
                key: UnitKey::new(&["conversation", "character", "conversation", "scriptstate"]).unwrap(),
                value: json!({"$__internal_ka_flag":"later","unrelated":{"keep":1}}),
            }]), ..Default::default() });
        }
        release.notify_one();
        let execution = finish(task).await;
        assert_eq!(phase(&execution), Phase::Completed, "{:?}", execution.diagnostic);
        assert_output_row(&harness, &ticket, &execution, if later_edit { "later()" } else { "true()" });
        assert_eq!(harness.store.generation_effects_evidence(&ticket).unwrap().unwrap(), evidence);
        assert_eq!(server.requests().len(), 1);
    }
}

#[tokio::test]
async fn prepared_variables_survive_preflight_failure_and_cancel_prevents_later_memory() {
    for mode in ["before-variables", "before-memory", "preflight"] {
        let server = serve(vec![reply(r#""unused""#, None)]).await;
        let mut harness = setup(&server.origin, json!({}), None);
        let ready = ready_with_variables(&mut harness);
        let (ticket, revision, memory) = (ready.ticket().clone(), harness.store.revision().unwrap(), harness.conversation().get("lastMemory").cloned());
        let execution = if mode == "preflight" {
            let production = OwnerLimits::production();
            let invalid = OwnerLimits { transport: super::ollama_transport::TransportLimits { max_request_bytes: 0, ..production.transport }, ..production };
            finish(harness.execute_with(ready, invalid)).await
        } else {
            let (entered, reached) = oneshot::channel();
            let (release, released) = mpsc::channel();
            let mut faults = vec![Some(Fault::Hold { entered, release: released })];
            if mode == "before-memory" { faults.insert(0, None); }
            harness.faults(faults);
            let task = harness.execute(ready);
            reached.await.unwrap();
            service::cancel_generation(&mut harness.store, &ticket).unwrap();
            release.send(()).unwrap();
            finish(task).await
        };
        assert_eq!(phase(&execution), if mode == "preflight" { Phase::Failed } else { Phase::Cancelled });
        assert_eq!(harness.conversation()["scriptstate"].get("$__internal_ka_flag").is_some(), mode != "before-variables");
        let evidence = harness.store.generation_effects_evidence(&ticket).unwrap().unwrap();
        let writes = match mode { "before-variables" => 0, "before-memory" => 1, _ => 2 };
        assert_eq!(harness.store.revision().unwrap(), revision + writes);
        assert_eq!(evidence.receipts.len(), writes as usize + 1);
        if mode != "preflight" { assert_eq!(harness.conversation().get("lastMemory"), memory.as_ref()); }
        assert_eq!((harness.evidence(&ticket).dispatch, server.requests().len(), harness.messages().len()), (DispatchState::None, 0, 3));
        service::BEFORE_PREPARATION.with(|hook| *hook.borrow_mut() = Some(Box::new(|| panic!("must not reevaluate cancelled or failed lore"))));
        assert!(matches!(service::prepare_ready_ollama(&mut harness.store, &ticket, &harness.host).unwrap(), ReadyPreparation::Existing(status) if status.phase != Phase::Running));
        service::BEFORE_PREPARATION.with(|hook| hook.borrow_mut().take());
        assert!(server.requests().is_empty());
    }
}

#[tokio::test]
async fn lost_variable_reply_resolves_exact_receipt_without_memory_http_or_replay() {
    let server = serve(vec![reply(r#""unused""#, None)]).await;
    let mut harness = setup(&server.origin, json!({}), None);
    let ready = ready_with_variables(&mut harness);
    let (ticket, revision, before) = (ready.ticket().clone(), harness.store.revision().unwrap(), harness.conversation());
    harness.faults(vec![Some(Fault::LoseReply)]);
    let execution = finish(harness.execute(ready)).await;
    assert_eq!(phase(&execution), Phase::Failed);
    assert_eq!(harness.evidence(&ticket).failure, failure("interrupted", Some("variables"), Some("preparation")));
    assert_eq!((harness.store.revision().unwrap(), harness.conversation()["scriptstate"]["$__internal_ka_flag"].clone()), (revision + 1, json!("true")));
    assert_eq!(harness.conversation().get("lastMemory"), before.get("lastMemory"));
    let evidence = harness.store.generation_effects_evidence(&ticket).unwrap().unwrap();
    assert_eq!((evidence.receipts.len(), evidence.receipts[1].revision), (2, revision + 1));
    harness.commit(WorkingSetCommit { unit_mutations: Some(vec![lww::UnitMutation::Set {
        key: UnitKey::new(&["conversation", "character", "conversation", "scriptstate"]).unwrap(), value: json!({"$__internal_ka_flag":"foreign"}),
    }]), ..Default::default() });
    assert_eq!(service::resolve_owner(&mut harness.store, &ticket).unwrap().phase, Phase::Failed);
    service::BEFORE_PREPARATION.with(|hook| *hook.borrow_mut() = Some(Box::new(|| panic!("must not reevaluate prepared lore"))));
    assert!(matches!(service::prepare_ready_ollama(&mut harness.store, &ticket, &harness.host).unwrap(), ReadyPreparation::Existing(status) if status.phase == Phase::Failed));
    service::BEFORE_PREPARATION.with(|hook| hook.borrow_mut().take());
    assert_eq!(harness.conversation()["scriptstate"]["$__internal_ka_flag"], "foreign");
    assert_eq!(harness.store.generation_effects_evidence(&ticket).unwrap().unwrap(), evidence);
    assert_eq!((harness.evidence(&ticket).dispatch, server.requests().len(), harness.messages().len()), (DispatchState::None, 0, 3));
}

#[tokio::test]
async fn prepared_lore_variables_reject_foreign_written_keys_and_merge_unrelated_values() {
    for same_key in [false, true] {
        let server = serve(vec![reply(r#""seed""#, None)]).await;
        let mut harness = output_harness(&server.origin, &json!({"character":{"customscript":[output_rule("seed", "{{getvar::__internal_ka_flag}}()")]}}));
        let ready = ready_with_variables(&mut harness);
        let (ticket, before) = (ready.ticket().clone(), harness.conversation());
        let (entered, reached) = oneshot::channel();
        let (release, released) = mpsc::channel();
        harness.faults(vec![Some(Fault::Hold { entered, release: released })]);
        let task = harness.execute(ready);
        reached.await.unwrap();
        let mut foreign = json!({"unrelated":{"keep":1}});
        if same_key { foreign["$__internal_ka_flag"] = json!("true"); }
        harness.commit(WorkingSetCommit { unit_mutations: Some(vec![lww::UnitMutation::Set {
            key: UnitKey::new(&["conversation", "character", "conversation", "scriptstate"]).unwrap(), value: foreign.clone(),
        }]), ..Default::default() });
        let revision = harness.store.revision().unwrap();
        release.send(()).unwrap();
        let execution = finish(task).await;
        service::BEFORE_PREPARATION.with(|hook| *hook.borrow_mut() = Some(Box::new(|| panic!("must not reevaluate settled lore"))));
        assert!(matches!(service::prepare_ready_ollama(&mut harness.store, &ticket, &harness.host).unwrap(), ReadyPreparation::Existing(status)
            if status.phase == if same_key { Phase::Stale } else { Phase::Completed }));
        service::BEFORE_PREPARATION.with(|hook| hook.borrow_mut().take());
        let evidence = harness.store.generation_effects_evidence(&ticket).unwrap().unwrap();
        if same_key {
            assert_eq!(phase(&execution), Phase::Stale);
            assert_eq!(harness.conversation()["scriptstate"], foreign);
            assert_eq!(harness.conversation().get("lastMemory"), before.get("lastMemory"));
            assert_eq!((harness.store.revision().unwrap(), evidence.receipts.len()), (revision, 1));
            assert_eq!((harness.evidence(&ticket).dispatch, server.requests().len(), harness.messages().len()), (DispatchState::None, 0, 3));
        } else {
            assert_eq!(phase(&execution), Phase::Completed, "{:?}", execution.diagnostic);
            foreign["$__internal_ka_flag"] = json!("true");
            assert_eq!(harness.conversation()["scriptstate"], foreign);
            assert_eq!(harness.conversation()["lastMemory"], "NewChat");
            assert_eq!(evidence.receipts.len(), 3);
            assert_output_row(&harness, &ticket, &execution, "true()");
            assert_eq!(server.requests().len(), 1);
        }
    }
}

fn commit_output_context_edit(harness: &mut Harness, variables: Value, defaults: Option<&Value>) {
    let mut mutations = vec![lww::UnitMutation::Set {
        key: UnitKey::new(&["conversation", "character", "conversation", "scriptstate"]).unwrap(), value:variables,
    }];
    let presets = defaults.map(|defaults| {
        mutations.push(lww::UnitMutation::Set {
            key: UnitKey::new(&["character", "character", "defaultVariables"]).unwrap(), value:defaults["characterDefaultVariables"].clone(),
        });
        let mut preset = harness.store.read_preset("preset-a", None).unwrap().unwrap().value;
        preset["templateDefaultVariables"] = defaults["presetTemplateDefaultVariables"].clone();
        vec![preset]
    });
    harness.commit(WorkingSetCommit { unit_mutations:Some(mutations), replace_presets:presets, ..Default::default() });
}

#[tokio::test]
async fn selected_module_lore_variables_settle_in_order_before_memory_and_http() {
    let release = Arc::new(Notify::new());
    let server = serve(vec![gated(&release, reply(r#""Module reply.""#, None))]).await;
    let mut harness = output_harness(&server.origin, &json!({
        "root":{"enabledModules":["A","B"],"modules":[
            {"id":"B","lorebook":[{"id":"module-B","key":"","alwaysActive":true,"insertorder":100,
                "content":"@@keep_activate_after_match\nORDER-B"}]},
            {"id":"A","lorebook":[{"id":"module-A","key":"","alwaysActive":true,"insertorder":100,
                "content":"@@keep_activate_after_match\nORDER-A sees={{getvar::__internal_ka_module-B}}."}]}]},
        "preset":{"moduleIntergration":""},"character":{"globalLore":[],"loreSettings":{"tokenBudget":1000,"scanDepth":10,"recursiveScanning":false}},
        "conversation":{"localLore":[]}
    }));
    let ready = harness.ready();
    let (ticket, revision) = (ready.ticket().clone(), harness.store.revision().unwrap());
    assert!(harness.conversation()["scriptstate"].get("$__internal_ka_module-B").is_none());
    assert!(harness.conversation()["scriptstate"].get("$__internal_ka_module-A").is_none());
    let task = harness.execute(ready);
    server.seen().await;
    assert_eq!(harness.conversation()["scriptstate"]["$__internal_ka_module-B"], "true");
    assert_eq!(harness.conversation()["scriptstate"]["$__internal_ka_module-A"], "true");
    assert_eq!(harness.conversation()["lastMemory"], "NewChat");
    let request: Value = serde_json::from_slice(&server.requests()[0].body).unwrap();
    let wire = request["messages"].as_array().unwrap().iter().map(|message| message["content"].as_str().unwrap()).collect::<Vec<_>>().join("\n");
    assert!(wire.contains("ORDER-A sees=true."), "{wire}");
    let effects = harness.store.generation_effects_evidence(&ticket).unwrap().unwrap();
    assert_eq!((effects.receipts[1].revision, effects.receipts[2].revision), (revision + 1, revision + 2));
    assert_eq!(effects.receipts.len(), 3);
    release.notify_one();
    let execution = finish(task).await;
    assert_eq!(phase(&execution), Phase::Completed, "{:?}", execution.diagnostic);
    assert_output_row(&harness, &ticket, &execution, "Module reply.");
    assert_eq!(harness.store.generation_effects_evidence(&ticket).unwrap().unwrap(), effects);
    assert_eq!(server.requests().len(), 1);
}

#[tokio::test]
async fn selected_module_output_reads_fresh_variables_and_defaults_without_replaying_lore() {
    let raw = raw_response_cases().into_iter().find(|raw| {
        let definition: ResponseCase = serde_json::from_str(raw.get()).unwrap();
        definition.id == "output-selected-module-fresh-defaults"
    }).unwrap();
    let case: Value = serde_json::from_str(raw.get()).unwrap();
    let release = Arc::new(Notify::new());
    let server = serve(vec![gated(&release, reply(&case["content"].to_string(), None))]).await;
    let mut harness = output_harness(&server.origin, &case);
    let mut ready = harness.ready();
    ready.pin_response_entropy(super::cbs::Entropy::pinned(now_millis() as f64, &[0.875]));
    let ticket = ready.ticket().clone();
    assert!(harness.conversation()["scriptstate"].get("$__internal_ka_module-fresh").is_none());
    let task = harness.execute(ready);
    server.seen().await;
    assert_eq!(harness.conversation()["scriptstate"]["$__internal_ka_module-fresh"], "true");
    assert_eq!(harness.conversation()["lastMemory"], "NewChat");
    let effects = harness.store.generation_effects_evidence(&ticket).unwrap().unwrap();
    let mut variables = harness.conversation()["scriptstate"].clone();
    for (name, value) in case["midflightVariables"].as_object().unwrap() {
        variables[format!("{}{}", "$", name)] = value.clone();
    }
    variables["$__internal_ka_module-fresh"] = json!("foreign-lore");
    commit_output_context_edit(&mut harness, variables.clone(), case.get("midflightDefaults"));
    release.notify_one();
    let execution = finish(task).await;
    assert_eq!(phase(&execution), Phase::Completed, "{:?}", execution.diagnostic);
    assert_output_row(&harness, &ticket, &execution, "new|new-character|new-template|foreign-new|foreign-lore|OUT-B");
    assert_eq!(harness.conversation()["scriptstate"], variables);
    assert_eq!(harness.store.generation_effects_evidence(&ticket).unwrap().unwrap(), effects);
    assert_eq!(server.requests().len(), 1);
}
