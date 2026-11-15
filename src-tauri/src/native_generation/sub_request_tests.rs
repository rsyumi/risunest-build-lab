use super::{
    ollama_decode::{tests as wire, Cancellation},
    ollama_transport::{self as transport, BufferedOllamaRequest, CapturedOllamaConfig, Think, TransportLimits},
    ollama_transport_tests::generation::loopback::{respond, serve, End, Reply},
    sub_request::{completed, execute, MemoryRequestConfig},
};
use risunest_ir::{chat::{PromptMessage, Role}, provider::{FailureCategory, FailureStage, OutputShape},
    stage::{HostReply, StageError, SubRequest}, Budget, Limits, Text, Value};
use serde_json::Value as Json;
use std::{sync::{atomic::AtomicBool, Arc}, time::Duration};

fn fixture() -> Json { serde_json::from_str(include_str!("../../../tests/fixtures/native-generation/ollama-memory.json")).unwrap() }
fn budget() -> Budget {
    Budget::new(Limits { max_owned_bytes: 8 << 20, max_json_bytes: 1 << 20, max_text_units: 1 << 20,
        max_values: 1 << 20, max_depth: 128, max_work: 32 << 20 }, Arc::new(AtomicBool::new(false))).unwrap()
}
fn limits() -> TransportLimits {
    TransportLimits { decode: wire::limits(), max_request_bytes: 65536, max_url_bytes: 4096, max_user_agent_bytes: 4096,
        max_buffered_bytes: 65536, max_line_bytes: 65536, max_output_bytes: 65536, max_output_value_units: 65536 }
}
fn units(value: &Json) -> Vec<u16> {
    value.as_array().unwrap().iter().map(|unit| u16::try_from(unit.as_u64().unwrap()).unwrap()).collect()
}
fn request(row: &Json, budget: &Budget) -> SubRequest {
    SubRequest { mode: Text::from_str("memory", budget).unwrap(),
        messages: row["messages"].as_array().unwrap().iter().map(|message| {
            let mut wire = message.clone();
            let fields = wire.as_object_mut().unwrap();
            let content_units = fields.remove("contentUnits");
            let name_units = fields.remove("nameUnits");
            if content_units.is_some() { fields.insert("content".into(), Json::String(String::new())); }
            if name_units.is_some() { fields.insert("name".into(), Json::String(String::new())); }
            let value = risunest_ir::parse_json(&wire.to_string(), budget).unwrap();
            let mut message = PromptMessage::from_value(value, budget).unwrap();
            if let Some(value) = content_units { message.content = Text::from_units(&units(&value), budget).unwrap(); }
            if let Some(value) = name_units { message.name = Some(Text::from_units(&units(&value), budget).unwrap()); }
            message
        }).collect(), options: risunest_ir::parse_json(r#"{"temperature":0.73,"maxTokens":777,"useStreaming":false}"#, budget).unwrap() }
}
fn response(spec: &Json) -> Reply {
    let status = spec["status"].as_u64().unwrap();
    respond(&format!("{status} {}", spec["reason"].as_str().unwrap()), &[("Content-Type", spec["contentType"].as_str().unwrap())],
        vec![spec["body"].as_str().unwrap().as_bytes().to_vec()], End::Finish)
}

fn config<'a>(host: &'a str, fixture: &'a Json, row: &Json, budget: &Budget) -> MemoryRequestConfig<'a> {
    MemoryRequestConfig { model_id: "ollama-hosted", user_name: Text::from_str(fixture["request"]["userName"].as_str().unwrap(), budget).unwrap(),
        ollama: CapturedOllamaConfig { host, model: fixture["request"]["model"].as_str().unwrap(),
            // The memory entry must force buffering even if a main-turn capture enables streaming.
            streaming: true, think: match row["thinkingMode"].as_str().unwrap() { "on" => Some(Think::Boolean(true)), "off" => Some(Think::Boolean(false)), _ => None },
            browser_user_agent: "Mozilla/5.0 (Synthetic RisuNest memory test)" } }
}

#[tokio::test]
async fn memory_subrequest_replays_actual_typescript_contracts_at_the_product_entry() {
    let fixture = fixture();
    for row in fixture["cases"].as_array().unwrap() {
        let id = row["id"].as_str().unwrap();
        let lifecycle = row["lifecycle"].as_str().unwrap_or("");
        // The deterministic post-provider cancellation boundary is exercised separately below.
        if lifecycle == "after-body-json" { continue; }
        let server = serve(|_, _| {
            let mut replies = vec![match lifecycle {
                "network-failure" | "pre-abort" => Reply::Close,
                "before-headers" => Reply::HoldHeaders,
                _ => response(&row["response"]),
            }];
            if let Some(spec) = row.get("reuseResponse") { replies.push(response(spec)); }
            replies
        }).await;
        let budget = budget();
        let request = request(row, &budget);
        let config = config(&server.origin, &fixture, row, &budget);
        let cancel = Cancellation::new();
        if lifecycle == "pre-abort" { cancel.cancel(); }
        let retained = budget.owned_bytes();
        let reply = tokio::time::timeout(Duration::from_secs(5), async {
            let pending = execute(&request, &config, limits(), &budget, &cancel);
            tokio::pin!(pending);
            if lifecycle == "before-headers" {
                tokio::select! {
                    _ = server.seen() => cancel.cancel(),
                    value = &mut pending => panic!("{id}: settled before cancellation: {value:?}"),
                }
            }
            pending.await
        }).await.unwrap_or_else(|_| panic!("{id}: external completion guard expired")).unwrap_or_else(|error| panic!("{id}: {error:?}"));
        let expected = &row["expected"];
        match &reply {
            HostReply::Llm(Ok(output)) if expected["kind"] == "resolved" && lifecycle != "pre-abort" => {
                assert!(matches!(output.shape, OutputShape::Text), "{id}: {:?}", output.shape);
                assert_eq!(output.texts.len(), 1, "{id}");
                let expected_units = expected["value"].get("resultUnits").map_or_else(
                    || expected["value"]["result"].as_str().unwrap().encode_utf16().collect(), units);
                assert_eq!(output.texts[0].units().collect::<Vec<_>>(), expected_units, "{id}");
                assert!(output.thoughts.is_empty() && output.tool_calls.is_empty() && output.usage.is_none() && output.finish.is_none(), "{id}");
                if row.get("reuseResponse").is_some() {
                    let next = SubRequest { mode: Text::from_str("memory", &budget).unwrap(), messages: vec![
                        PromptMessage::new(Role::User, output.texts[0].copy(&budget).unwrap()),
                        PromptMessage::new(Role::System, Text::from_str("Summarize again.", &budget).unwrap()),
                    ], options: Value::Undefined };
                    let reused = tokio::time::timeout(Duration::from_secs(5), execute(&next, &config, limits(), &budget, &cancel)).await.unwrap().unwrap();
                    assert!(matches!(reused, HostReply::Llm(Ok(ref value)) if value.texts.len() == 1
                        && value.texts[0].as_unicode() == expected["reuseValue"]["result"].as_str()), "{id}: {reused:?}");
                }
            }
            HostReply::Llm(Err(error)) if lifecycle == "pre-abort" => {
                assert_eq!(error.category, FailureCategory::Cancelled);
                assert_eq!(error.stage, Some(FailureStage::Request));
                assert_eq!(error.sdk_message.as_ref().and_then(Text::as_unicode), Some("Aborted"));
                assert!(error.http_status.is_none() && error.body_failure.is_none());
            }
            HostReply::Failed { message } if expected["kind"] == "thrown" => {
                let actual = message.as_unicode().unwrap();
                match id {
                    "malformed-json" => assert!(actual.starts_with("Invalid JSON at byte "), "{actual}"),
                    "network-failure" => assert_eq!(actual, "Failed to fetch"),
                    "caller-cancel-inflight" => assert_eq!(actual, "This operation was aborted"),
                    id if id.starts_with("coercion-") && expected["name"] == "TypeError" =>
                        assert_eq!(actual, "Cannot convert object to primitive value", "{id}"),
                    _ => assert_eq!(actual, expected["message"].as_str().unwrap(), "{id}"),
                }
            }
            other => panic!("{id}: unexpected reply {other:?}"),
        }
        let requests = server.requests();
        assert_eq!(requests.len(), expected["requestCount"].as_u64().unwrap() as usize, "{id}");
        for (index, captured) in requests.iter().enumerate() {
            assert_eq!((captured.method.as_str(), captured.path.as_str()), ("POST", "/api/chat"), "{id}");
            let field = if index == 0 { "bodyText" } else { "reuseBodyText" };
            assert_eq!(std::str::from_utf8(&captured.body).unwrap(), expected[field].as_str().unwrap(), "{id}");
        }
        if lifecycle == "before-headers" { server.client_closed().await; }
        drop(reply);
        assert_eq!(budget.owned_bytes(), retained, "{id}: request, transport and reply charges must release");
    }
}

#[tokio::test]
async fn memory_subrequest_declines_unowned_modes_and_models_before_precancel_or_http() {
    let fixture = fixture();
    let row = &fixture["cases"][1];
    let server = serve(|_, _| vec![Reply::Close]).await;
    let budget = budget();
    let mut request = request(row, &budget);
    let cancel = Cancellation::new(); cancel.cancel();
    let mut config = config(&server.origin, &fixture, row, &budget);
    for (mode, model) in [("memory", "gpt35"), ("emotion", "ollama-hosted"), ("submodel", "ollama-hosted")] {
        request.mode = Text::from_str(mode, &budget).unwrap(); config.model_id = model;
        let retained = budget.owned_bytes();
        let reply = tokio::time::timeout(Duration::from_secs(5), execute(&request, &config, limits(), &budget, &cancel)).await.unwrap().unwrap();
        assert!(matches!(reply, HostReply::Unsupported(ref name) if name == "memory-model"), "{reply:?}");
        assert_eq!(budget.owned_bytes(), retained);
        assert!(server.requests().is_empty());
    }
}

#[tokio::test]
async fn memory_subrequest_enforces_request_decoding_and_formatted_reply_limits() {
    let fixture = fixture();
    let row = &fixture["cases"][1];
    for bound in ["request", "buffered", "output-bytes", "output-units"] {
        let server = serve(|_, _| vec![respond("200 OK", &[("Content-Type", "application/json")],
            vec![br#"{"message":{"content":"long summary"}}"#.to_vec()], End::Finish)]).await;
        let budget = budget();
        let request = request(row, &budget);
        let config = config(&server.origin, &fixture, row, &budget);
        let cancel = Cancellation::new();
        let mut limits = limits();
        match bound { "request" => limits.max_request_bytes = 1, "buffered" => limits.max_buffered_bytes = 1,
            "output-bytes" => limits.max_output_bytes = 1, _ => limits.max_output_value_units = 1 }
        let retained = budget.owned_bytes();
        let result = tokio::time::timeout(Duration::from_secs(5), execute(&request, &config, limits, &budget, &cancel)).await.unwrap();
        assert!(matches!(result, Err(StageError::Limit(_))), "{bound}: {result:?}");
        assert_eq!(server.requests().len(), usize::from(bound != "request"), "{bound}");
        assert_eq!(budget.owned_bytes(), retained, "{bound}: failed call retains no charges");
    }
}

#[tokio::test]
async fn memory_subrequest_uses_the_callers_owned_budget_before_copying_or_http() {
    let fixture = fixture();
    let row = &fixture["cases"][1];
    let server = serve(|_, _| vec![Reply::Close]).await;
    let inputs = budget();
    let request = request(row, &inputs);
    let config = config(&server.origin, &fixture, row, &inputs);
    let maximum = Budget::context_bytes() + 64;
    let mut constrained = Limits { max_owned_bytes: 8 << 20, max_json_bytes: 1 << 20, max_text_units: 1 << 20,
        max_values: 1 << 20, max_depth: 128, max_work: 32 << 20 };
    constrained.max_owned_bytes = maximum;
    let budget = Budget::new(constrained, Arc::new(AtomicBool::new(false))).unwrap();
    let cancel = Cancellation::new();
    let result = tokio::time::timeout(Duration::from_secs(5), execute(&request, &config, limits(), &budget, &cancel)).await.unwrap();
    assert!(matches!(result, Err(StageError::Limit(_))), "{result:?}");
    assert!(server.requests().is_empty());
    assert_eq!(budget.owned_bytes(), Budget::context_bytes());
    assert!(budget.peak_bytes() <= maximum);
}

#[tokio::test]
async fn memory_subrequest_returns_aborted_after_a_successful_provider_without_throwing() {
    let fixture = fixture();
    let row = fixture["cases"].as_array().unwrap().iter().find(|row| row["lifecycle"] == "after-body-json").unwrap();
    let server = serve(|_, _| vec![response(&row["response"])]).await;
    let budget = budget();
    let request = request(row, &budget);
    let config = config(&server.origin, &fixture, row, &budget);
    let cancel = Cancellation::new();
    let body = risunest_ir::parse_json(row["expected"]["bodyText"].as_str().unwrap(), &budget).unwrap();
    let aborted = Text::from_str("Aborted", &budget).unwrap();
    let wire = BufferedOllamaRequest { body: &body, messages: &request.messages, user_name: &config.user_name };
    let value = tokio::time::timeout(Duration::from_secs(5), transport::buffered(wire, &config.ollama, limits(), &budget, &cancel)).await.unwrap().unwrap();
    assert!(matches!(&value, Value::Text(value) if value.as_unicode() == Some("completed summary")));
    // Cancel only after the real provider returned, then run the same completion mapper execute uses.
    cancel.cancel();
    let reply = completed(value, &cancel, aborted).unwrap();
    assert!(matches!(reply, HostReply::Llm(Err(ref error)) if error.category == FailureCategory::Cancelled
        && error.stage == Some(FailureStage::Request) && error.http_status.is_none() && error.body_failure.is_none()
        && error.sdk_message.as_ref().and_then(Text::as_unicode) == Some("Aborted")), "{reply:?}");
    assert_eq!(server.requests().len(), 1);
    assert_eq!(std::str::from_utf8(&server.requests()[0].body).unwrap(), row["expected"]["bodyText"].as_str().unwrap());
}
