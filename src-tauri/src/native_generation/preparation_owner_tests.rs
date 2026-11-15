use super::*;
use crate::{native_generation::{cbs::Entropy, prepare::{PreparedSend, prepare_ollama_owned}, owner::OwnerLimits},
    persistent_store::generation::preparation_tests::{database, stored}};
use serde_json::{json, Value};
use std::{future::Future, task::Poll, time::Duration};

fn limits() -> http::Limits {
    http::Limits { url_bytes: 4096, header_bytes: 65536, response_bytes: 1 << 20,
        decode_owned: 1 << 20, decode_work: 1 << 26, copy_quantum: 4096,
        decode: crate::native_generation::ollama_decode::tests::limits() }
}

fn operation_fixture() -> (Operation, Arc<Semaphore>, std::sync::Weak<()>) {
    let slots = Arc::new(Semaphore::new(1));
    let live = Arc::new(());
    let weak = Arc::downgrade(&live);
    (Operation::new_with_liveness(slots.clone(), prepare::PREPARATION_LIMITS, live).unwrap(), slots, weak)
}

fn snapshot(patch: &Value) -> PreparationSnapshot {
    let (_directory, mut store, intent) = stored(&database(patch));
    let mut snapshot = store.generation_preparation_snapshot(&intent).unwrap();
    snapshot.entropy = Some(Entropy::pinned(1_700_000_000_000.0, &[0.25, 0.75]));
    snapshot
}

fn result(attempt: PreparationAttempt) -> (Result<Value, PreparationError>, super::super::preparation_effects::PreparedVariablePlan) {
    (attempt.result.map(|value| match value {
        PreparedSend::Turn(turn) => serde_json::to_value(turn).unwrap(),
        PreparedSend::Stopped => json!({"stopped":true}),
    }), attempt.variables)
}

#[tokio::test]
async fn owned_preparation_matches_sync_prompt_prefix_stop_and_failures() {
    for (patch, stopped, failure) in [
        (json!({"preset":{"mainPrompt":"{{random::FIRST::SECOND}}"}}), false, None),
        (json!({"preset":{"maxContext":1,"maxResponse":0}}), false, Some(PreparationError::ContextLimit)),
        (json!({"root":{"statics":{"messages":"invalid"}}}), false, Some(PreparationError::Unsupported("statics".into()))),
        (json!({"character":{"triggerscript":[{"comment":"","type":"start","conditions":[],"effect":[
            {"type":"v2SetVar","operator":"=","var":"beforeFailure","valueType":"value","value":"kept","indent":0}]}]},
            "messages":[{"role":"user","chatId":"tokenizer-after-trigger","data":"<|endoftext|>"}]}),
            false, Some(PreparationError::Tokenizer("disallowed_special_token".into()))),
        (json!({"character":{"triggerscript":[{"comment":"","type":"start","conditions":[],"effect":[
            {"type":"v2SetVar","operator":"=","var":"stopped","valueType":"value","value":"true","indent":0},
            {"type":"v2StopPromptSending","indent":0}]}]}}), true, None),
    ] {
        let mut snapshot = snapshot(&patch);
        snapshot.admitted_stop = stopped;
        let expected = result(prepare_ollama_owned(&snapshot));
        assert_eq!(expected.0.as_ref().err(), failure.as_ref(), "{patch}");
        let (operation, slots, weak) = operation_fixture();
        let resources = Resources::no_memory(operation, OwnerLimits::production().transport).unwrap();
        assert!(resources.executor.is_none() && resources.memory.is_none() && resources.stop.is_none());
        let completion = prepare(snapshot, resources).await;
        assert_eq!(slots.available_permits(), 0);
        assert!(weak.upgrade().is_some());
        let (value, retention) = completion.into_parts();
        let retained_budget = retention.budget();
        let CompletionValue::Prepared { snapshot, attempt } = value else { panic!("owned preparation failed outside the core") };
        let actual = result(attempt);
        assert_eq!(actual, expected, "{patch}");
        if matches!(failure, Some(PreparationError::Tokenizer(_))) {
            assert!(actual.1.assignments.iter().any(|(name, value)| name == "beforeFailure" && value == "kept"));
        }
        if stopped { assert!(actual.1.assignments.iter().any(|(name, value)| name == "stopped" && value == "true")); }
        assert!(retained_budget.owned_bytes() > Budget::context_bytes());
        assert!(weak.upgrade().is_some());
        drop(actual); drop(snapshot);
        drop(retention);
        assert_eq!(retained_budget.owned_bytes(), Budget::context_bytes());
        // Payload charges do not own the lifecycle; the final operation does.
        assert_eq!(slots.available_permits(), 1);
        assert!(weak.upgrade().is_none());
    }
}

// Registrations contain only weak control identity. A task owns its own gate once selected.
struct RegisteredGate { key: std::sync::Weak<AtomicBool>, skip: usize, phase: u8, gate: StepGate }
fn step_gates() -> &'static std::sync::Mutex<Vec<RegisteredGate>> {
    static GATES: std::sync::OnceLock<std::sync::Mutex<Vec<RegisteredGate>>> = std::sync::OnceLock::new();
    GATES.get_or_init(Default::default)
}
pub(in crate::native_generation) struct GateRegistration { key: std::sync::Weak<AtomicBool> }
impl Drop for GateRegistration {
    fn drop(&mut self) { step_gates().lock().unwrap().retain(|entry| !entry.key.ptr_eq(&self.key)); }
}
pub(super) struct StepGate {
    after: bool,
    entered: Option<tokio::sync::oneshot::Sender<Budget>>,
    release: std::sync::mpsc::Receiver<()>,
}
impl StepGate {
    fn pause(&mut self, budget: &Budget) {
        self.entered.take().unwrap().send(budget.clone()).ok();
        self.release.recv_timeout(Duration::from_secs(30)).unwrap();
    }
    pub(super) async fn admission(&mut self, budget: &Budget) {
        self.entered.take().unwrap().send(budget.clone()).ok();
        tokio::time::timeout(Duration::from_secs(30), async move {
            loop {
                match self.release.try_recv() {
                    Ok(()) => break,
                    Err(std::sync::mpsc::TryRecvError::Empty) => tokio::task::yield_now().await,
                    Err(error) => panic!("admission gate disconnected: {error}"),
                }
            }
        }).await.unwrap();
    }
    pub(super) fn before(&mut self, budget: &Budget) { if !self.after { self.pause(budget); } }
    pub(super) fn after(&mut self, budget: &Budget) { if self.after { self.pause(budget); } }
}
pub(super) fn take_step_gate(cancel: &Cancellation, phase: u8) -> Option<StepGate> {
    let mut gates = step_gates().lock().unwrap();
    gates.retain(|entry| entry.key.strong_count() > 0);
    let key = Arc::downgrade(&cancel.flag());
    let index = gates.iter().position(|entry| entry.key.ptr_eq(&key) && entry.phase == phase)?;
    if gates[index].skip > 0 { gates[index].skip -= 1; return None; }
    Some(gates.remove(index).gate)
}
pub(in crate::native_generation) fn gate_service_step(cancel: &Cancellation, skip: usize, after: bool)
    -> (GateRegistration, tokio::sync::oneshot::Receiver<Budget>, std::sync::mpsc::Sender<()>) {
    register_service_gate(cancel, skip, after, 0)
}
pub(in crate::native_generation) fn gate_service_admission(cancel: &Cancellation, skip: usize)
    -> (GateRegistration, tokio::sync::oneshot::Receiver<Budget>, std::sync::mpsc::Sender<()>) {
    register_service_gate(cancel, skip, false, 1)
}
pub(in crate::native_generation) fn observe_service_queued(cancel: &Cancellation)
    -> (GateRegistration, tokio::sync::oneshot::Receiver<Budget>) {
    let (registration, ready, release) = register_service_gate(cancel, 0, false, 2);
    release.send(()).unwrap();
    (registration, ready)
}
fn register_service_gate(cancel: &Cancellation, skip: usize, after: bool, phase: u8)
    -> (GateRegistration, tokio::sync::oneshot::Receiver<Budget>, std::sync::mpsc::Sender<()>) {
    let key = Arc::downgrade(&cancel.flag());
    let (entered, ready) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    step_gates().lock().unwrap().push(RegisteredGate { key: key.clone(), skip, phase, gate: StepGate { after, entered: Some(entered), release: wait } });
    (GateRegistration { key }, ready, release)
}

// The barrier surrounds a genuine blocking step, never replaces its result.
fn gated(task: StepTask, after: bool) -> (JoinHandle<StepOutput>, tokio::sync::oneshot::Receiver<()>, std::sync::mpsc::Sender<()>) {
    let (started, ready) = tokio::sync::oneshot::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let handle = tokio::task::spawn_blocking(move || {
        if after {
            let output = task.run();
            started.send(()).unwrap();
            wait.recv_timeout(Duration::from_secs(10)).unwrap();
            output
        } else {
            started.send(()).unwrap();
            wait.recv_timeout(Duration::from_secs(10)).unwrap();
            task.run()
        }
    });
    (handle, ready, release)
}

async fn pending<T>(future: std::pin::Pin<&mut impl Future<Output = T>>) {
    let mut future = future;
    assert!(std::future::poll_fn(|cx| Poll::Ready(future.as_mut().poll(cx).is_pending())).await);
}

#[tokio::test]
async fn hard_stop_joins_running_step_and_held_output_before_releasing_owner() {
    for after in [false, true] {
        let (operation, slots, weak) = operation_fixture();
        let task = StepTask::new(Work::Build(snapshot(&json!({}))), &operation).ok().unwrap();
        let (handle, started, release) = gated(task, after);
        started.await.unwrap();
        operation.cancel().cancel_as(CancelOrigin::Owner);
        let mut settled = Box::pin(settle_step(handle, &operation));
        pending(settled.as_mut()).await;
        assert_eq!(slots.available_permits(), 0);
        assert!(weak.upgrade().is_some());
        release.send(()).unwrap();
        let SettledStep::Stopped { output: Some(output), error: OwnerError::Http(Failure::Cancelled) } = settled.await
            else { panic!("hard cancellation must retain the actual joined output") };
        if after { assert!(matches!(output.value, TaskValue::Built { .. })); }
        else { assert!(matches!(output.value, TaskValue::Rejected { error: Failure::Cancelled, .. })); }
        drop(operation);
        assert_eq!(slots.available_permits(), 0);
        assert!(weak.upgrade().is_some());
        drop(output);
        assert_eq!(slots.available_permits(), 1);
        assert!(weak.upgrade().is_none());
    }
}

#[tokio::test]
async fn abandoning_running_step_signals_hard_but_retains_actual_work_until_cleanup() {
    for after in [false, true] {
        let (operation, slots, weak) = operation_fixture();
        let task = StepTask::new(Work::Build(snapshot(&json!({}))), &operation).ok().unwrap();
        let (handle, started, release) = gated(task, after);
        started.await.unwrap();
        let mut settled = Box::pin(settle_step(handle, &operation));
        pending(settled.as_mut()).await;
        drop(settled);
        assert!(matches!(operation.check(), Err(Failure::Cancelled)));
        drop(operation);
        assert_eq!(slots.available_permits(), 0);
        assert!(weak.upgrade().is_some());
        release.send(()).unwrap();
        // Aborting a running blocking job discards its real output only after the closure returns.
        tokio::time::timeout(Duration::from_secs(10), async {
            while slots.available_permits() == 0 || weak.upgrade().is_some() { tokio::task::yield_now().await; }
        }).await.unwrap();
        assert!(weak.upgrade().is_none());
    }
}

#[test]
fn queued_step_cancellation_releases_only_the_aborted_job_and_never_starts_it() {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().max_blocking_threads(1).build().unwrap();
    runtime.block_on(async {
        let (hold, wait) = std::sync::mpsc::channel();
        let (started, ready) = tokio::sync::oneshot::channel();
        let blocker = tokio::task::spawn_blocking(move || { started.send(()).unwrap(); wait.recv_timeout(Duration::from_secs(10)).unwrap(); });
        ready.await.unwrap();
        let (operation, slots, weak) = operation_fixture();
        let task = StepTask::new(Work::Build(snapshot(&json!({}))), &operation).ok().unwrap();
        let ran = Arc::new(AtomicBool::new(false));
        let worker_ran = ran.clone();
        let handle = tokio::task::spawn_blocking(move || {
            worker_ran.store(true, std::sync::atomic::Ordering::SeqCst);
            task.run()
        });
        operation.cancel().cancel_as(CancelOrigin::Owner);
        let mut settled = Box::pin(settle_step(handle, &operation));
        pending(settled.as_mut()).await;
        assert_eq!(slots.available_permits(), 0);
        assert!(weak.upgrade().is_some());
        assert!(!ran.load(std::sync::atomic::Ordering::SeqCst));
        hold.send(()).unwrap();
        assert!(matches!(settled.await,
            SettledStep::Stopped { output: None, error: OwnerError::Http(Failure::Cancelled) }));
        blocker.await.unwrap();
        assert!(!ran.load(std::sync::atomic::Ordering::SeqCst));
        drop(operation);
        assert_eq!(slots.available_permits(), 1);
        assert!(weak.upgrade().is_none());
    });
}


fn isolate_loopback(qualified: &str) -> bool {
    const CHILD: &str = "RISUNEST_PREPARATION_LOOPBACK_CHILD";
    let exact = qualified.split_once("::").unwrap().1;
    if std::env::var(CHILD).as_deref() == Ok(exact) { return false; }
    let mut child = std::process::Command::new(std::env::current_exe().unwrap());
    child.args(["--exact", exact, "--nocapture", "--test-threads=1"]);
    for name in ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy", "REQUEST_METHOD"] { child.env_remove(name); }
    child.env("NO_PROXY", "*,127.0.0.0/8,::1").env("no_proxy", "*,127.0.0.0/8,::1").env(CHILD, exact);
    let output = child.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{exact}: {stdout}\n{}", String::from_utf8_lossy(&output.stderr));
    assert!(stdout.lines().rev().find(|line| line.starts_with("test result:")).is_some_and(|line|
        line.starts_with("test result: ok. 1 passed; 0 failed;")), "exact child did not run: {stdout}");
    print!("{stdout}");
    true
}

use crate::native_generation::ollama_transport_tests::generation::loopback::{serve, respond, End, Reply};
use crate::persistent_store::{PersistentStore, commands::{PersistentStoreState, hypa::native_cache::NativeCache}};
use risunest_memory::{hanurai::{Hanurai, HanuraiConfig, Retrieval}, hypa_v1::EmbeddingTarget};
use risunest_providers::embeddings::CapturedConfig;
use risunest_tokenizer::{counter::{Counter, ChatAccounting}, id::TokenizerId};

fn counter() -> Counter { Counter::new(TokenizerId::Cl100kBase, ChatAccounting::default()) }

fn memory_snapshot(hanurai: bool) -> PreparationSnapshot {
    let messages: Vec<_> = (0..if hanurai {4} else {12}).map(|index| json!({"role": if index % 2 == 0 {"user"} else {"char"},
        "chatId":format!("owned-{index}"),"data":format!("Synthetic memory {index}. {}", "word ".repeat(48))})).collect();
    let mut preset = json!({"maxContext":if hanurai {2048} else {220},"maxResponse":16});
    if hanurai { preset["promptSettings"] = json!({"trimStartNewChat":true}); }
    snapshot(&json!({"root":{"hanuraiEnable":hanurai,"supaModelType":if hanurai {"none"} else {"subModel"}},
        "preset":preset,"character":{"supaMemory":true,"firstMessage":""},"messages":messages}))
}

fn retrieval(state: Arc<PersistentStoreState>, notify: Arc<dyn Fn() + Send + Sync>, origin: &str, operation: &Operation)
    -> impl Retrieval + Send + 'static {
    let budget = operation.budget();
    let target = EmbeddingTarget::capture(&Text::from_str("custom", budget).unwrap(), None,
        &Text::from_str(&format!("{origin}/v1/embeddings"), budget).unwrap(), &Text::from_str("owned-embed", budget).unwrap(), budget).unwrap();
    crate::native_generation::memory_retrieval::PolicyRetrieval::new(target, NativeCache::new(state, notify),
        |budget: &Budget| CapturedConfig::capture(None, None, None, None, budget), 64, operation).unwrap()
}

fn hanurai_core(snapshot: &PreparationSnapshot, state: Arc<PersistentStoreState>, notify: Arc<dyn Fn() + Send + Sync>,
    origin: &str, operation: &Operation) -> MachineCore {
    let stage = Hanurai::new(HanuraiConfig { split: false, reserve_tokens: 100.0,
        context_limit_error: Text::from_str("synthetic context limit", operation.budget()).unwrap() }, counter(),
        retrieval(state, notify, origin, operation));
    prepare::lower_machine(snapshot, operation.budget(), Box::new(stage)).ok().unwrap()
}

fn embedding_reply() -> Reply {
    respond("200 OK", &[("Content-Type", "application/json")],
        vec![serde_json::to_vec(&json!({"data":[{"embedding":[1.0,0.0]}]})).unwrap()], End::Finish)
}

#[tokio::test]
async fn whole_core_hanurai_stalled_http_releases_store_and_hard_stop_keeps_unresumed_owner() {
    if isolate_loopback(concat!(module_path!(), "::whole_core_hanurai_stalled_http_releases_store_and_hard_stop_keeps_unresumed_owner")) { return; }
    let directory = tempfile::tempdir().unwrap();
    let state = Arc::new(PersistentStoreState::with_test_store(PersistentStore::open(directory.path()).unwrap()));
    let server = serve(|_, _| vec![Reply::HoldHeaders]).await;
    let (operation, slots, weak) = operation_fixture();
    let snapshot = memory_snapshot(true);
    assert_eq!(prepare::admit_ollama(&snapshot).unwrap_err(), PreparationError::Unsupported("memory".into()));
    let core = hanurai_core(&snapshot, state.clone(), Arc::new(|| {}), &server.origin, &operation);
    let resources = Resources::new(operation.clone(), limits(), OwnerLimits::production().transport, None, None).unwrap();
    let mut owner = Box::pin(run_core(core, resources));
    tokio::select! { biased;
        _ = server.seen() => {},
        _ = &mut owner => panic!("held HTTP unexpectedly completed"),
    }
    // An independent genuine store job remains available while the whole core is suspended on HTTP.
    let access = state.admit_native_job_operation().unwrap();
    let mut store = state.open_admitted_native_job_store(&access).unwrap();
    assert!(store.revision().is_ok());
    drop(store); drop(access);
    operation.cancel().cancel_as(CancelOrigin::Owner);
    let completion = owner.await;
    assert!(matches!(completion.value, CompletionValue::Failed { error: OwnerError::Http(Failure::Cancelled), state: FailureState::Core(_) }));
    server.client_closed().await;
    assert_eq!(server.requests().len(), 1);
    drop(operation);
    assert_eq!(slots.available_permits(), 0);
    assert!(weak.upgrade().is_some());
    drop(completion);
    assert_eq!(slots.available_permits(), 1);
    assert!(weak.upgrade().is_none());

    // A composition without an HTTP owner refuses a real embedding dependency without dispatch or panic.
    let (operation, slots, weak) = operation_fixture();
    let core = hanurai_core(&snapshot, state, Arc::new(|| {}), &server.origin, &operation);
    let resources = Resources::no_memory(operation.clone(), OwnerLimits::production().transport).unwrap();
    let completion = run_core(core, resources).await;
    let CompletionValue::Raw(raw) = &completion.value else { panic!("unselected dependency must resume through host refusal") };
    assert!(matches!(&raw.outcome, Ok(risunest_prompt::machine::PromptOutcome::Failed {
        reason: risunest_prompt::machine::PromptFailure::Unsupported(name), .. }) if name == "embedding-http-host"));
    assert_eq!(raw.diagnostic(), Some(PreparationError::Unsupported("embedding-http-host".into())));
    assert_eq!(server.requests().len(), 1);
    drop(operation); drop(completion);
    assert_eq!(slots.available_permits(), 1);
    assert!(weak.upgrade().is_none());

}

#[tokio::test]
async fn whole_core_hanurai_real_http_completes_without_enabling_app_memory() {
    if isolate_loopback(concat!(module_path!(), "::whole_core_hanurai_real_http_completes_without_enabling_app_memory")) { return; }
    let directory = tempfile::tempdir().unwrap();
    let state = Arc::new(PersistentStoreState::with_test_store(PersistentStore::open(directory.path()).unwrap()));
    let server = serve(|_, _| (0..8).map(|_| embedding_reply()).collect()).await;
    let (operation, slots, weak) = operation_fixture();
    let snapshot = memory_snapshot(true);
    assert_eq!(prepare::admit_ollama(&snapshot).unwrap_err(), PreparationError::Unsupported("memory".into()));
    let core = hanurai_core(&snapshot, state, Arc::new(|| {}), &server.origin, &operation);
    let completion = run_core(core, Resources::new(operation.clone(), limits(), OwnerLimits::production().transport, None, None).unwrap()).await;
    let CompletionValue::Raw(raw) = &completion.value else { panic!("whole lower core failed") };
    assert!(matches!(raw.outcome, Ok(risunest_prompt::machine::PromptOutcome::Ready(_))));
    assert!(raw.effects.is_empty());
    let requests = server.requests();
    assert_eq!(requests.len(), 4, "document batch and three genuine Hanurai query calls");
    assert!(requests.iter().all(|request| request.method == "POST" && request.path == "/v1/embeddings"));
    let first_history = format!("search_document: {}", snapshot.history.messages[0]["data"].as_str().unwrap().trim());
    for (index, request) in requests.iter().enumerate() {
        let wire: Value = serde_json::from_slice(&request.body).unwrap();
        if index == 0 { assert_eq!(wire["input"], json!([first_history])); }
        else { assert!(wire["input"].as_str().is_some_and(|query| query.starts_with("search_query: "))); }
    }
    drop(operation); drop(completion);
    assert_eq!(slots.available_permits(), 1);
    assert!(weak.upgrade().is_none());
}

#[tokio::test]
async fn http_resume_distinguishes_rejected_callback_from_real_completion_and_outer_hard_error() {
    if isolate_loopback(concat!(module_path!(), "::http_resume_distinguishes_rejected_callback_from_real_completion_and_outer_hard_error")) { return; }
    for reject in [true, false] {
        let directory = tempfile::tempdir().unwrap();
        let state = Arc::new(PersistentStoreState::with_test_store(PersistentStore::open(directory.path()).unwrap()));
        let server = serve(|_, _| vec![embedding_reply()]).await;
        let (operation, slots, _) = operation_fixture();
        let cancel = operation.cancel().clone();
        let notify = Arc::new(move || { cancel.cancel_as(CancelOrigin::Owner); });
        let snapshot = memory_snapshot(true);
        let core = hanurai_core(&snapshot, state, notify, &server.origin, &operation);
        let CoreYield::NeedsHost { core, request: HostRequest::Http(request) } = core.start(operation.budget())
            else { panic!("genuine Hanurai document embedding request") };
        let wire: Value = serde_json::from_slice(&request.body).unwrap();
        let first_history = format!("search_document: {}", snapshot.history.messages[0]["data"].as_str().unwrap().trim());
        assert_eq!(wire["input"], json!([first_history]), "the singleton reply must match the actual first-history document batch");
        let mut executor = Executor::new(operation.clone(), limits()).unwrap();
        let foreign = if reject { Some(operation_fixture().0) } else { None };
        let reply = if let Some(foreign) = &foreign {
            Executor::new(foreign.clone(), limits()).unwrap().execute(request, None).await.unwrap()
        } else { executor.execute(request, None).await.unwrap() };
        operation.check().unwrap();
        assert!(!operation.cancel().is_cancelled());
        let task = StepTask::new(Work::ResumeHttp { core, executor, reply }, &operation).ok().unwrap();
        let output = tokio::task::spawn_blocking(move || task.run()).await.unwrap();
        match &output.value {
            TaskValue::Http { result: HttpResume::Rejected { error: Failure::Invalid("native-http-reply-owner"), .. }, .. } if reject => {},
            TaskValue::Http { result: HttpResume::Resumed { progress: CoreYield::Done(raw), outer: Err(Failure::Cancelled) }, .. } if !reject => {
                assert!(matches!(raw.outcome, Err(StageError::Cancelled)));
                assert!(operation.cancel().is_cancelled());
                assert_eq!(operation.cancel().origin(), CancelOrigin::Owner);
            }
            _ => panic!("callback rejection and actual inner completion must remain distinct (reject={reject}, hard_cancelled={})",
                operation.cancel().is_cancelled()),
        }
        drop(foreign); drop(output); drop(operation);
        assert_eq!(slots.available_permits(), 1);
    }
}

fn supa_core(snapshot: &PreparationSnapshot, state: Arc<PersistentStoreState>, origin: &str, operation: &Operation)
    -> (MachineCore, MemoryConfig) {
    use risunest_memory::{supa::{SupaMemory, SupaMemoryConfig}, summary::Summary};
    let budget = operation.budget();
    let input = prepare::prompt_input(snapshot, budget).unwrap();
    let processor = crate::native_generation::scripts::SharedText::new(crate::native_generation::cbs::processor(
        &input, budget, &crate::native_generation::cbs::Draws::Fresh, None).unwrap());
    let summary = Summary::supa(&Text::from_str("subModel", budget).unwrap(), &Text::from_str("", budget).unwrap(), None, processor, budget).unwrap();
    let stage = SupaMemory::new(SupaMemoryConfig { as_hyper: false, remove_punctuation: false, max_chunk_size: 128.0,
        char_name: Text::from_str("Synthetic character", budget).unwrap(), user_name: Text::from_str("Synthetic user", budget).unwrap() },
        None, counter(), retrieval(state, Arc::new(|| {}), origin, &operation), summary);
    let core = prepare::lower_machine(snapshot, budget, Box::new(stage)).ok().unwrap();
    let config = MemoryConfig::capture("ollama-hosted", &Text::from_str("Synthetic user", budget).unwrap(), CapturedOllamaConfig {
        host: origin, model: "owned-submodel", streaming: false, think: None, browser_user_agent: "synthetic owner" }, budget).unwrap();
    (core, config)
}

#[tokio::test]
async fn whole_core_supa_uses_real_summary_subrequest_and_returns_actual_effects() {
    if isolate_loopback(concat!(module_path!(), "::whole_core_supa_uses_real_summary_subrequest_and_returns_actual_effects")) { return; }
    // This selects only the lower driver's forwarding contract, independently of product memory mode policy.
    for selected in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let state = Arc::new(PersistentStoreState::with_test_store(PersistentStore::open(directory.path()).unwrap()));
        let server = serve(|_, _| (0..24).map(|_| respond("200 OK", &[("Content-Type","application/json")],
            vec![serde_json::to_vec(&json!({"message":{"content":"OWNED_SUMMARY"}})).unwrap()], End::Finish)).collect()).await;
        let (operation, slots, weak) = operation_fixture();
        let snapshot = memory_snapshot(false);
        assert_eq!(prepare::admit_ollama(&snapshot).unwrap_err(), PreparationError::Unsupported("memory".into()));
        let (core, config) = supa_core(&snapshot, state, &server.origin, &operation);
        let user = Cancellation::new();
        user.cancel_as(CancelOrigin::Caller);
        let resources = Resources::new(operation.clone(), limits(), OwnerLimits::production().transport, Some(config),
            selected.then(|| user.clone())).unwrap();
        let completion = run_core(core, resources).await;
        operation.check().unwrap();
        assert!(!operation.cancel().is_cancelled());
        let CompletionValue::Raw(raw) = &completion.value else { panic!("whole Supa core failed outside actual terminal outcome") };
        let requests = server.requests();
        if selected {
            assert!(matches!(&raw.outcome, Ok(risunest_prompt::machine::PromptOutcome::Failed {
                reason: risunest_prompt::machine::PromptFailure::Memory(message), ..
            }) if message.as_unicode() == Some("SupaMemory: HTTP: Aborted")));
            assert!(raw.effects.is_empty());
            assert!(requests.is_empty(), "pre-aborted selected control must refuse HTTP");
        } else {
            assert!(raw.effects.iter().any(|effect| matches!(effect, risunest_ir::stage::Effect::ChatField { field, value: risunest_ir::Value::Text(value) }
                if field.equals_ascii(b"supaMemoryData") && value.as_unicode().is_some_and(|value| value.contains("OWNED_SUMMARY")))));
            assert!(!requests.is_empty(), "unforwarded User control must allow the genuine summary");
            assert!(requests.iter().all(|request| request.path == "/api/chat"));
            let wire: Value = serde_json::from_slice(&requests[0].body).unwrap();
            assert_eq!(wire["model"], "owned-submodel");
            assert_eq!(wire["stream"], false);
        }
        drop(operation);
        assert_eq!(slots.available_permits(), 0);
        assert!(weak.upgrade().is_some());
        drop(completion);
        assert_eq!(slots.available_permits(), 1);
        assert!(weak.upgrade().is_none());
    }
}


#[tokio::test]
async fn whole_core_supa_hard_stop_joins_real_subrequest_without_inventing_completion() {
    if isolate_loopback(concat!(module_path!(), "::whole_core_supa_hard_stop_joins_real_subrequest_without_inventing_completion")) { return; }
    for control in ["hard", "selected-soft", "unforwarded-soft"] {
        let directory = tempfile::tempdir().unwrap();
        let state = Arc::new(PersistentStoreState::with_test_store(PersistentStore::open(directory.path()).unwrap()));
        let server = serve(|_, _| vec![Reply::HoldHeaders]).await;
        let (operation, slots, weak) = operation_fixture();
        let (core, config) = supa_core(&memory_snapshot(false), state.clone(), &server.origin, &operation);
        let user = Cancellation::new();
        let resources = Resources::new(operation.clone(), limits(), OwnerLimits::production().transport, Some(config),
            (control == "selected-soft").then(|| user.clone())).unwrap();
        let mut owner = Box::pin(run_core(core, resources));
        tokio::select! { biased;
            _ = server.seen() => {},
            _ = &mut owner => panic!("held summary unexpectedly completed"),
        }
        let access = state.admit_native_job_operation().unwrap();
        let mut store = state.open_admitted_native_job_store(&access).unwrap();
        assert!(store.revision().is_ok());
        drop(store); drop(access);
        if control == "hard" { operation.cancel().cancel_as(CancelOrigin::Owner); }
        else { user.cancel_as(CancelOrigin::Caller); }
        if control == "unforwarded-soft" {
            pending(owner.as_mut()).await;
            operation.check().unwrap();
            assert!(!operation.cancel().is_cancelled());
            // This lower owner deliberately did not select the User control. Hard cleanup is still joined.
            operation.cancel().cancel_as(CancelOrigin::Owner);
        }
        let completion = owner.await;
        if control == "selected-soft" {
            let CompletionValue::Raw(raw) = &completion.value else { panic!("selected request stop poisoned the hard owner") };
            assert!(matches!(&raw.outcome, Err(StageError::Host(message))
                if message == &"This operation was aborted".encode_utf16().collect::<Vec<_>>()));
            assert!(raw.effects.is_empty());
            operation.check().unwrap();
            assert!(!operation.cancel().is_cancelled());
        } else {
            assert!(matches!(completion.value, CompletionValue::Failed { error: OwnerError::Http(Failure::Cancelled), state: FailureState::Core(_) }));
        }
        server.client_closed().await;
        assert_eq!(server.requests().len(), 1);
        drop(operation);
        assert_eq!(slots.available_permits(), 0);
        assert!(weak.upgrade().is_some());
        drop(completion);
        assert_eq!(slots.available_permits(), 1);
        assert!(weak.upgrade().is_none());
    }
}

#[tokio::test]
async fn exhausted_step_admission_keeps_unrun_snapshot_and_owner_without_fabricated_prefix() {
    // This capacity admits the NoMemory wrapper, but cannot hold an unused decoder reserve.
    let slots = Arc::new(Semaphore::new(1));
    let small = Operation::new_with_liveness(slots.clone(), risunest_ir::Limits {
        max_owned_bytes: 64 * 1024, ..prepare::PREPARATION_LIMITS
    }, Arc::new(())).unwrap();
    let resources = Resources::no_memory(small.clone(), OwnerLimits::production().transport).unwrap();
    assert!(resources.executor.is_none());
    drop(resources); drop(small);
    assert_eq!(slots.available_permits(), 1);
    let (operation, slots, weak) = operation_fixture();
    let resources = Resources::new(operation.clone(), limits(), OwnerLimits::production().transport, None, None).unwrap();
    let remaining = prepare::PREPARATION_LIMITS.max_owned_bytes - operation.budget().owned_bytes();
    let held = operation.budget().reserve_owned(remaining).unwrap();
    let completion = prepare(snapshot(&json!({})), resources).await;
    assert!(matches!(completion.value, CompletionValue::Failed { error: OwnerError::Http(Failure::Limit("owned-bytes")),
        state: FailureState::Work(Work::Build(_)) }));
    drop(held); drop(operation);
    assert_eq!(slots.available_permits(), 0);
    assert!(weak.upgrade().is_some());
    drop(completion);
    assert_eq!(slots.available_permits(), 1);
    assert!(weak.upgrade().is_none());

    // Owned wrapper reservations cannot consume this text limit before the genuine snapshot conversion.
    let slots = Arc::new(Semaphore::new(1));
    let live = Arc::new(());
    let weak = Arc::downgrade(&live);
    let operation = Operation::new_with_liveness(slots.clone(), risunest_ir::Limits {
        max_text_units: 1, ..prepare::PREPARATION_LIMITS
    }, live).unwrap();
    let resources = Resources::new(operation.clone(), limits(), OwnerLimits::production().transport, None, None).unwrap();
    let completion = prepare(snapshot(&json!({})), resources).await;
    let CompletionValue::Prepared { attempt, .. } = &completion.value else { panic!("supplied text limit must return an actual preparation failure") };
    assert_eq!(attempt.result.as_ref().unwrap_err(), &PreparationError::Invalid("limit:text-units".into()));
    assert_eq!(attempt.variables, crate::native_generation::preparation_effects::PreparedVariablePlan::default());
    operation.check().unwrap();
    assert!(!operation.cancel().is_cancelled());
    drop(operation);
    assert_eq!(slots.available_permits(), 0);
    assert!(weak.upgrade().is_some());
    drop(completion);
    assert_eq!(slots.available_permits(), 1);
    assert!(weak.upgrade().is_none());
}


#[tokio::test]
async fn hard_stop_after_blocking_terminal_conversion_retains_real_prefix_until_output_drop() {
    let (operation, slots, weak) = operation_fixture();
    let snapshot = snapshot(&json!({}));
    let prepare::OwnedStart { names, progress, .. } = prepare::start_owned(&snapshot, operation.budget()).unwrap();
    let CoreYield::Done(raw) = progress else { panic!("NoMemory preparation must finish without host requests") };
    let task = StepTask::new(Work::Finalize { raw, snapshot, names }, &operation).ok().unwrap();
    let (handle, started, release) = gated(task, true);
    started.await.unwrap();
    operation.cancel().cancel_as(CancelOrigin::Owner);
    let mut settled = Box::pin(settle_step(handle, &operation));
    pending(settled.as_mut()).await;
    release.send(()).unwrap();
    let SettledStep::Stopped { output: Some(output), error: OwnerError::Http(Failure::Cancelled) } = settled.await
        else { panic!("hard cancellation must retain the actual converted outcome") };
    assert!(matches!(&output.value, TaskValue::Finalized { attempt: PreparationAttempt { result: Ok(PreparedSend::Turn(_)), .. }, .. }));
    drop(operation);
    assert_eq!(slots.available_permits(), 0);
    assert!(weak.upgrade().is_some());
    drop(output);
    assert_eq!(slots.available_permits(), 1);
    assert!(weak.upgrade().is_none());
}

#[tokio::test]
async fn panicked_real_step_is_a_join_failure_without_invented_terminal_result() {
    let (operation, slots, weak) = operation_fixture();
    let task = StepTask::new(Work::Build(snapshot(&json!({}))), &operation).ok().unwrap();
    let handle = tokio::task::spawn_blocking(move || {
        let _output = task.run();
        panic!("synthetic owner task panic after actual preparation");
    });
    assert!(matches!(settle_step(handle, &operation).await, SettledStep::Stopped { output: None, error: OwnerError::Join }));
    assert_eq!(slots.available_permits(), 0);
    assert!(weak.upgrade().is_some());
    drop(operation);
    assert_eq!(slots.available_permits(), 1);
    assert!(weak.upgrade().is_none());
}
