//! Compares raw provider requests and decoded values against actual HypaProcesser observations.
use risunest_ir::stage::{HostReply, HostRequest, Step};
use risunest_ir::{Budget, Limits, OwnedVec, Text};
use risunest_providers::embeddings::*;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::{Arc, atomic::AtomicBool};

fn budget() -> Budget { Budget::new(Limits { max_owned_bytes: 8 << 20, max_json_bytes: 1 << 20, max_text_units: 1 << 20,
    max_values: 100_000, max_depth: 128, max_work: 10_000_000 }, Arc::new(AtomicBool::new(false))).unwrap() }
fn units(value: &Value) -> Vec<u16> { value.as_array().unwrap().iter().map(|n| n.as_u64().unwrap() as u16).collect() }
fn text(value: &Value, b: &Budget) -> Text { Text::from_units(&units(value), b).unwrap() }
fn optional(value: &Value, b: &Budget) -> Option<Text> { value.get("u").map(|value| text(value, b)) }
fn source(value: &str, b: &Budget) -> Text { Text::from_str(value, b).unwrap() }
fn update(settings: &mut Value, patch: &Value) { if let Some(patch) = patch.as_object() { for (key, value) in patch { settings[key] = value.clone(); } } }
fn number(value: &Value) -> f64 { match value.as_str() { Some("NaN") => f64::NAN, Some("Infinity") => f64::INFINITY,
    Some("-Infinity") => f64::NEG_INFINITY, Some("-0") => -0.0, _ => value.as_f64().unwrap() } }

#[test]
fn real_typescript_embedding_requests_and_numeric_replies_match() {
    let input_bytes = include_bytes!("../../../../tests/fixtures/rust-core/providers/embeddings/remote.cases.json");
    let expected: Value = serde_json::from_slice(include_bytes!("../../../../tests/fixtures/rust-core/providers/embeddings/remote.expected.json")).unwrap();
    assert_eq!(expected["fixtureSha256"], Sha256::digest(input_bytes).iter().map(|byte| format!("{byte:02x}")).collect::<String>());
    let inputs: Value = serde_json::from_slice(input_bytes).unwrap();
    let cases = inputs["cases"].as_array().unwrap();
    assert!(cases.len() >= 6);
    assert_eq!(cases.len(), expected["cases"].as_array().unwrap().len());
    for (case, observation) in cases.iter().zip(expected["cases"].as_array().unwrap()) {
        assert_eq!(case["id"], observation["id"]);
        let b = budget();
        let input = &case["input"];
        let construct = &input["construct"];
        let mut settings = construct["settings"].clone();
        let mut override_key = json!({"absent":true});
        let calls = input["calls"].as_array().unwrap();
        let observed_calls = observation["result"]["calls"].as_array().unwrap();
        assert_eq!(calls.len(), observed_calls.len(), "{}", case["id"]);
        assert!(!calls.is_empty());
        for (call, observed) in calls.iter().zip(observed_calls) {
            update(&mut settings, &call["before"]);
            if let Some(value) = call.get("oaikey") { override_key = value.clone(); }
            let configured = construct["settings"]["hypaModel"].as_str().map(|value| source(value, &b));
            let override_url = optional(&construct["customUrl"], &b);
            let configured_url = optional(&construct["settings"]["customUrl"], &b);
            let target = RemoteTarget::capture(&source(construct["model"].as_str().unwrap(), &b), configured.as_ref(), override_url.as_ref(), configured_url.as_ref(), &b).unwrap();
            let model = optional(&settings["customModel"], &b); let key = optional(&settings["customKey"], &b);
            let openai = optional(&override_key, &b); let fallback = optional(&settings["supaKey"], &b);
            let config = CapturedConfig::capture(model.as_ref(), key.as_ref(), openai.as_ref(), fallback.as_ref(), &b).unwrap();
            let input = match call["input"]["shape"].as_str().unwrap() {
                "scalar" => Input::Scalar(text(&call["input"]["text"], &b)),
                "batch" => { let mut values = OwnedVec::new(&b).unwrap(); for value in call["input"]["texts"].as_array().unwrap() { values.push(text(value, &b)).unwrap(); } Input::Batch(values) },
                _ => panic!(),
            };
            let purpose = if call["purpose"] == "document" { Purpose::Document } else { Purpose::Query };
            let mut service = Service::new();
            let start = service.run(Request { target, config, purpose, input }, &b);
            let raw = observed["raw"].as_array().unwrap();
            if raw.is_empty() {
                assert!(matches!(start, Err(Failure::Invalid("embedding-custom-url"))), "{}", case["id"]);
                assert!(observed["outcome"].get("failure").is_some());
                assert!(observed["wire"].as_array().unwrap().is_empty());
            } else {
                assert_eq!(raw.len(), 1);
                let Step::NeedsHost(HostRequest::Http(http)) = start.unwrap() else { panic!() };
                assert_eq!(http.url.encode_utf16().collect::<Vec<_>>(), units(&raw[0]["constructedUrlUsv"]), "{}", case["id"]);
                assert_eq!(std::str::from_utf8(&http.body).unwrap().encode_utf16().collect::<Vec<_>>(), units(&raw[0]["bodyText"]), "{}", case["id"]);
                let mut headers = vec![("Content-Type".to_owned(), "application/json".to_owned())];
                headers.extend(raw[0]["headers"].as_array().unwrap().iter().map(|pair| (String::from_utf16(&units(&pair[0])).unwrap(), String::from_utf16(&units(&pair[1])).unwrap())));
                assert_eq!(http.headers, headers, "{}", case["id"]);
                assert_eq!(http.method, risunest_ir::provider::HttpMethod::Post);
                assert_eq!(http.response, risunest_ir::provider::ResponseKind::Buffered);
                drop(http);
                // Invalid source URLs have raw observations only. Native URL parsing belongs to a later owner.
                if call["sourceBoundary"] == "before-transport" {
                    assert!(observed["wire"].as_array().unwrap().is_empty());
                    assert!(observed["outcome"].get("failure").is_some());
                    assert_eq!(call["failureComparison"], "incidental-url");
                } else {
                    assert_eq!(observed["wire"].as_array().unwrap().len(), 1);
                    let reply = &call["reply"];
                    let reply = if reply["kind"] == "transport" { HostReply::Failed { message: text(&reply["message"], &b) } }
                        else { HostReply::Http { status: reply["status"].as_u64().unwrap() as u16, body: reply["bytes"].as_array().unwrap().iter().map(|n| n.as_u64().unwrap() as u8).collect() } };
                    let result = service.resume(reply, &b);
                    if call["nativeComparison"] == "reject-unsupported-shape" {
                        assert!(matches!(result, Err(Failure::Decode { .. })), "{}", case["id"]);
                    } else if let Some(vectors) = observed["outcome"].get("vectors") {
                        let Step::Done(actual) = result.unwrap() else { panic!() };
                        let vectors = vectors.as_array().unwrap();
                        assert_eq!(actual.as_slice().len(), vectors.len(), "{}", case["id"]);
                        for (actual, expected) in actual.as_slice().iter().zip(vectors) {
                            if expected.is_null() || expected.get("undefined").is_some() { assert!(actual.is_none()); }
                            else {
                                let actual = actual.as_ref().unwrap().as_slice(); let expected = expected.as_array().unwrap();
                                assert_eq!(actual.len(), expected.len());
                                for (actual, expected) in actual.iter().zip(expected) { assert_eq!(actual.to_bits(), number(expected).to_bits(), "{}", case["id"]); }
                            }
                        }
                    } else {
                        let failure = result.unwrap_err();
                        match call["failureComparison"].as_str() {
                            Some("exact-remote-json") => {
                                let Failure::Remote { diagnostic, .. } = failure else { panic!("{}", case["id"]) };
                                assert_eq!(diagnostic.units().collect::<Vec<_>>(), units(&observed["outcome"]["failure"]["value"]["u"]), "{}", case["id"]);
                            }
                            Some("incidental-transport") => assert!(matches!(failure, Failure::Transport { .. })),
                            Some("incidental-decode" | "incidental-shape") => assert!(matches!(failure, Failure::Decode { .. })),
                            _ => panic!("unclassified failure {}", case["id"]),
                        }
                    }
                    assert!(matches!(service.resume(HostReply::Cancelled, &b), Err(Failure::Invalid("embedding-resume-state"))));
                }
            }
            drop(service);
            update(&mut settings, &call["duringReply"]);
        }
        assert_eq!(b.owned_bytes(), Budget::context_bytes(), "{}", case["id"]);
    }
}
