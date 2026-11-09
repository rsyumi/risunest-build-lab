use super::*;
use serde_json::json;

fn ticket() -> Ticket { serde_json::from_value(json!({ "epoch": "synthetic", "sequence": "1" })).unwrap() }
fn session(gate: Gate, lease: Duration) -> Arc<Session> {
    Arc::new(Session::new(Token { boot: "synthetic-boot".into(), generation: 1, ticket: ticket(), gate }, lease))
}
async fn entered(session: &Session) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while session.snapshot().holds != 1 { tokio::task::yield_now().await; }
    }).await.expect("gate did not enter");
}

#[test]
fn identity_and_retirement_protect_later_sessions() {
    let controller = Controller::default();
    let token = controller.arm(ticket(), Gate::Admitted).unwrap();
    let first = controller.session(&token).unwrap();
    assert!(controller.arm(ticket(), Gate::None).is_err());
    for changed in [Token { boot: "other".into(), ..token.clone() },
        Token { generation: token.generation + 1, ..token.clone() },
        Token { gate: Gate::Credit, ..token.clone() },
        Token { ticket: Ticket { epoch: "other".into(), ..ticket() }, ..token.clone() }] {
        assert!(controller.session(&changed).is_err());
    }
    assert!(first.release().is_err());
    let actor = controller.capture(&ticket()).unwrap();
    first.retire();
    assert!(controller.capture(&ticket()).is_none());
    assert!(!first.snapshot().drained);
    assert_eq!(first.snapshot().invalid, Some("retire-before-drain"));
    assert!(controller.arm(ticket(), Gate::None).is_err());
    drop(actor);
    let next = controller.arm(ticket(), Gate::None).unwrap();
    assert_eq!(next.generation, token.generation + 1);
    assert!(controller.session(&token).is_err());
    assert!(Controller::default().session(&next).is_err());
    assert!(controller.capture(&Ticket { epoch: "wrong".into(), ..ticket() }).is_none());
}

#[tokio::test]
async fn dropped_async_hold_never_releases_a_later_session() {
    let session = session(Gate::Credit, HOLD_LEASE);
    let actor = session.enroll().unwrap();
    let captured = session.clone();
    let task = tokio::spawn(async move { captured.wait(Gate::Credit).await; });
    entered(&session).await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    assert_eq!(session.snapshot().holds, 0);
    assert!(session.snapshot().events.iter().any(|event| event.kind == Kind::GateDropped));
    assert!(session.release().is_err());
    drop(actor);
    session.retire();
    let snapshot = session.snapshot();
    assert!(snapshot.retired && snapshot.drained);
    assert_eq!(snapshot.invalid, None);
}

#[tokio::test]
async fn synchronous_holds_release_retire_and_expire_without_owning_controller_lock() {
    for retire in [false, true] {
        let session = session(Gate::Admitted, HOLD_LEASE);
        let captured = session.clone();
        let actor = session.enroll().unwrap();
        let task = tokio::task::spawn_blocking(move || {
            captured.wait_blocking(Gate::Admitted);
            drop(actor);
        });
        entered(&session).await;
        if retire { session.retire(); } else { session.release().unwrap(); }
        tokio::time::timeout(Duration::from_secs(5), task).await.unwrap().unwrap();
        let snapshot = session.snapshot();
        assert!(snapshot.drained);
        assert_eq!(snapshot.invalid, retire.then_some("forced-unblock"));
        assert!(session.release().is_err());
    }
    let session = session(Gate::PreAdmission, Duration::ZERO);
    session.wait_blocking(Gate::PreAdmission);
    assert_eq!(session.snapshot().invalid, Some("lease-expired"));
    assert!(session.snapshot().drained);
}

#[tokio::test]
async fn async_lease_expiry_is_invalid_and_drain_includes_hooks() {
    let session = session(Gate::Offer, Duration::ZERO);
    let hook = session.hook();
    session.wait(Gate::Offer).await;
    assert_eq!(session.snapshot().invalid, Some("lease-expired"));
    assert!(!session.snapshot().drained);
    drop(hook);
    assert!(session.snapshot().drained);
}

#[test]
fn retirement_with_only_a_hook_is_invalid_until_that_hook_drains() {
    let session = session(Gate::None, HOLD_LEASE);
    let hook = session.hook();
    session.retire();
    assert_eq!(session.snapshot().invalid, Some("retire-before-drain"));
    assert!(!session.snapshot().drained);
    drop(hook);
    assert!(session.snapshot().drained);
    assert_eq!(session.snapshot().invalid, Some("retire-before-drain"));
}

#[test]
fn trace_rejects_duplicate_out_of_order_and_unbounded_observations() {
    let first = session(Gate::None, HOLD_LEASE);
    first.record(Kind::JsonParsed, None);
    assert_eq!(first.snapshot().invalid, Some("out-of-order-event"));
    let duplicate = session(Gate::None, HOLD_LEASE);
    duplicate.record(Kind::RequestFormatted, None);
    duplicate.record(Kind::RequestFormatted, None);
    assert_eq!(duplicate.snapshot().invalid, Some("duplicate-event"));
    for _ in 0..TRACE_LIMIT { duplicate.record(Kind::RequestFormatted, None); }
    assert_eq!(duplicate.snapshot().events.len(), TRACE_LIMIT);
}

fn request(command: &str, body: serde_json::Value) -> tauri::webview::InvokeRequest {
    tauri::webview::InvokeRequest {
        cmd: command.into(), callback: tauri::ipc::CallbackFn(0), error: tauri::ipc::CallbackFn(1),
        url: "http://tauri.localhost".parse().unwrap(), body: tauri::ipc::InvokeBody::Json(body),
        headers: Default::default(), invoke_key: tauri::test::INVOKE_KEY.to_string(),
    }
}
fn invoke(webview: &tauri::WebviewWindow<tauri::test::MockRuntime>, command: &str, body: serde_json::Value)
    -> Result<serde_json::Value, serde_json::Value> {
    let (tx, rx) = std::sync::mpsc::channel();
    webview.as_ref().clone().on_message(request(command, body), Box::new(move |_, _, response, _, _| {
        tx.send(response).unwrap();
    }));
    match rx.recv_timeout(Duration::from_secs(5)).unwrap() {
        tauri::ipc::InvokeResponse::Ok(body) => Ok(body.deserialize().unwrap()),
        tauri::ipc::InvokeResponse::Err(error) => Err(error.0),
    }
}

#[test]
fn actual_handler_registers_controls_and_refuses_an_unmanaged_builder() {
    let app = tauri::test::mock_builder().invoke_handler(handler())
        .build(tauri::test::mock_context(tauri::test::noop_assets())).unwrap();
    let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default()).build().unwrap();
    assert_eq!(invoke(&webview, "ollama_ui_arm", json!({"ticket": ticket(), "gate": "none"})),
        Err(json!("native-ui:controller-unavailable")));
    app.manage(Controller::default());
    let token: Token = serde_json::from_value(invoke(&webview, "ollama_ui_arm",
        json!({"ticket": ticket(), "gate": "credit"})).unwrap()).unwrap();
    assert_eq!(invoke(&webview, "ollama_ui_snapshot", json!({"token": token})).unwrap()["holds"], 0);
    assert_eq!(invoke(&webview, "ollama_ui_release", json!({"token": token})), Err(json!("native-ui:hold-not-owned")));
    let retired = invoke(&webview, "ollama_ui_retire", json!({"token": token})).unwrap();
    assert_eq!((retired["retired"].clone(), retired["drained"].clone()), (json!(true), json!(true)));
    let next = invoke(&webview, "ollama_ui_arm", json!({"ticket": ticket(), "gate": "none"})).unwrap();
    assert_ne!(next["generation"], token.generation);
    assert_eq!(invoke(&webview, "ollama_ui_snapshot", json!({"token": token})), Err(json!("native-ui:stale-token")));
}

#[test]
fn harness_command_futures_are_send_and_fit_the_ipc_budget() {
    fn check(value: impl std::future::Future + Send) { assert!(std::mem::size_of_val(&value) <= 4096); }
    let app = tauri::test::mock_builder().build(tauri::test::mock_context(tauri::test::noop_assets())).unwrap();
    let token = session(Gate::None, HOLD_LEASE).token.clone();
    check(ollama_ui_arm(app.handle().clone(), ticket(), Gate::None));
    check(ollama_ui_snapshot(app.handle().clone(), token.clone()));
    check(ollama_ui_release(app.handle().clone(), token.clone()));
    check(ollama_ui_retire(app.handle().clone(), token));
}
