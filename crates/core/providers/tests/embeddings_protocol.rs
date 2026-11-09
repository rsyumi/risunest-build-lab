use risunest_ir::stage::{HostReply, HostRequest, Step};
use risunest_ir::{Budget, Limits, OwnedVec, Text};
use risunest_providers::embeddings::*;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

fn budget(owned: usize, work: usize, cancel: Arc<AtomicBool>) -> Budget {
    Budget::new(Limits { max_owned_bytes: owned, max_json_bytes: 1 << 20, max_text_units: 1 << 20,
        max_values: 100_000, max_depth: 128, max_work: work }, cancel).unwrap()
}
fn ample() -> Budget { budget(4 << 20, 10_000_000, Arc::new(AtomicBool::new(false))) }
fn text(value: &str, b: &Budget) -> Text { Text::from_str(value, b).unwrap() }
fn config(b: &Budget) -> CapturedConfig { CapturedConfig::capture(None, None, None, None, b).unwrap() }
fn request(b: &Budget) -> Request { Request { target: RemoteTarget::Ada, config: config(b), purpose: Purpose::Query, input: Input::Scalar(text("q", b)) } }
fn http(service: &mut Service, request: Request, b: &Budget) -> risunest_ir::provider::HttpRequestSpec {
    let Step::NeedsHost(HostRequest::Http(http)) = service.run(request, b).unwrap() else { panic!() }; http
}
fn reply(service: &mut Service, status: u16, body: &[u8], b: &Budget) -> Result<Step<Output>, Failure> {
    service.resume(HostReply::Http { status, body: body.to_vec() }, b)
}

#[test]
fn request_capture_preserves_custom_shape_and_redacts_nested_secrets() {
    let b = ample();
    let target = RemoteTarget::capture(&text("custom", &b), None, Some(&text(" https:////user:secret@host/v1?x#frag ", &b)), None, &b).unwrap();
    let config = CapturedConfig::capture(Some(&text(" new-model ", &b)), Some(&text(" fresh-key ", &b)), None, None, &b).unwrap();
    let mut input = OwnedVec::new(&b).unwrap();
    input.push(Text::from_units(&[0x61, 0xd800], &b).unwrap()).unwrap();
    let req = Request { target, config, purpose: Purpose::Document, input: Input::Batch(input) };
    for secret in ["secret", "fresh-key", "new-model", "user", "host"] { assert!(!format!("{req:?}").contains(secret)); }
    let mut service = Service::new();
    let http = http(&mut service, req, &b);
    assert_eq!(http.url, "https:////user:secret@host/v1?x#frag/embeddings");
    assert_eq!(http.headers, [("Content-Type".into(), "application/json".into()), ("Authorization".into(), "Bearer fresh-key".into())]);
    assert_eq!(http.body, br#"{"input":["a\ud800"],"model":"new-model"}"#);
    assert!(!format!("{http:?}").contains("secret"));
    assert!(!format!("{service:?}").contains("secret"));
    drop(http);
    assert!(matches!(reply(&mut service, 200, br#"{"data":[]}"#, &b), Ok(Step::Done(_))));
    drop(service);
    assert_eq!(b.owned_bytes(), Budget::context_bytes());
}

#[test]
fn openai_mapping_preserves_absent_blank_and_override_credentials() {
    for (name, model) in [("ada", "text-embedding-ada-002"), ("openai3small", "text-embedding-3-small"), ("openai3large", "text-embedding-3-large")] {
        for (override_key, fallback, expected) in [(None, None, "Bearer undefined"), (None, Some(" "), "Bearer "), (Some(" "), Some(" old "), "Bearer old"), (Some(" new "), Some(" old "), "Bearer new")] {
            let b = ample();
            let override_key = override_key.map(|value| text(value, &b));
            let fallback = fallback.map(|value| text(value, &b));
            let config = CapturedConfig::capture(None, None, override_key.as_ref(), fallback.as_ref(), &b).unwrap();
            let target = RemoteTarget::capture(&text(name, &b), None, None, None, &b).unwrap();
            let mut service = Service::new();
            let http = http(&mut service, Request { target, config, purpose: Purpose::Query, input: Input::Scalar(text("q", &b)) }, &b);
            assert_eq!(http.url, "https://api.openai.com/v1/embeddings");
            assert_eq!(http.headers[1].1, expected);
            assert_eq!(String::from_utf8(http.body).unwrap(), format!("{{\"input\":\"q\",\"model\":\"{model}\"}}"));
        }
    }
}

#[test]
fn replies_keep_server_order_optional_vectors_and_f64_values() {
    let b = ample();
    let mut service = Service::new();
    drop(http(&mut service, request(&b), &b));
    let Step::Done(output) = reply(&mut service, 299, br#"{"data":[{"index":4,"embedding":[1.00000001,-0,1e999]},{"index":0},{"embedding":null},{"embedding":[]},{"embedding":[2]}]}"#, &b).unwrap() else { panic!() };
    assert_eq!(output.as_slice().len(), 5);
    let first = output.as_slice()[0].as_ref().unwrap().as_slice();
    assert_eq!(first[0].to_bits(), 1.00000001f64.to_bits());
    assert_eq!(first[1].to_bits(), (-0f64).to_bits());
    assert_eq!(first[2], f64::INFINITY);
    assert!(output.as_slice()[1].is_none());
    assert!(output.as_slice()[2].is_none());
    assert!(output.as_slice()[3].as_ref().unwrap().as_slice().is_empty());
    assert_eq!(output.as_slice()[4].as_ref().unwrap().as_slice(), [2.0]);
    drop(service);
    assert!(b.owned_bytes() > Budget::context_bytes());
    drop(output);
    assert_eq!(b.owned_bytes(), Budget::context_bytes());
}

#[test]
fn errors_retain_owned_diagnostics_and_do_not_retry() {
    for (status, body) in [(400, br#"{"2":"two","1":"one","error":{"message":"secret"},"extra":-0}"#.as_slice()), (200, b"{"), (503, b"{"), (200, br#"{"data":null}"#), (200, br#"{"data":[null]}"#), (200, br#"{"data":[{"embedding":["1"]}]}"#)] {
        let b = ample();
        let mut service = Service::new();
        drop(http(&mut service, request(&b), &b));
        let failure = reply(&mut service, status, body, &b).unwrap_err();
        assert!(!format!("{failure:?}").contains("secret"));
        if status == 400 {
            let Failure::Remote { diagnostic, status } = &failure else { panic!() };
            assert_eq!(*status, 400);
            assert_eq!(diagnostic.as_unicode(), Some(r#"{"1":"one","2":"two","error":{"message":"secret"},"extra":0}"#));
        } else { assert!(matches!(failure, Failure::Decode { .. })); }
        assert!(matches!(service.resume(HostReply::Cancelled, &b), Err(Failure::Invalid("embedding-resume-state"))));
        assert!(matches!(service.run(request(&b), &b), Err(Failure::Invalid("embedding-run-state"))));
        drop(service);
        assert!(b.owned_bytes() > Budget::context_bytes());
        drop(failure);
        assert_eq!(b.owned_bytes(), Budget::context_bytes());
    }
    let b = ample();
    let mut service = Service::new();
    drop(http(&mut service, request(&b), &b));
    let failure = service.resume(HostReply::Failed { message: text("transport-secret", &b) }, &b).unwrap_err();
    assert!(!format!("{failure:?}").contains("transport-secret"));
    assert!(matches!(&failure, Failure::Transport { diagnostic } if diagnostic.as_unicode() == Some("transport-secret")));
    drop(service); drop(failure);
    assert_eq!(b.owned_bytes(), Budget::context_bytes());
}

#[test]
fn pending_request_charge_and_cancelled_resume_release_owned_state() {
    for cancel_pending in [false, true] {
        let cancel = Arc::new(AtomicBool::new(false));
        let b = budget(1 << 20, 1_000_000, cancel.clone());
        let mut service = Service::new();
        let mut req = request(&b);
        req.input = Input::Scalar(text(&"x".repeat(4097), &b));
        let http = http(&mut service, req, &b);
        let raw = http.url.capacity() + http.body.capacity() + http.headers.capacity() * std::mem::size_of::<(String, String)>()
            + http.headers.iter().map(|(name, value)| name.capacity() + value.capacity()).sum::<usize>();
        assert!(b.owned_bytes() >= Budget::context_bytes() + raw);
        drop(http);
        assert!(b.owned_bytes() >= Budget::context_bytes() + raw);
        if cancel_pending {
            let message = text("cancelled-transport-secret", &b);
            cancel.store(true, Ordering::Relaxed);
            assert!(matches!(service.resume(HostReply::Failed { message }, &b), Err(Failure::Cancelled)));
        } else { assert!(matches!(service.run(request(&b), &b), Err(Failure::Invalid("embedding-run-state")))); }
        drop(service);
        assert_eq!(b.owned_bytes(), Budget::context_bytes());
    }
}

#[test]
fn raw_reply_capacity_is_admitted_before_decoding() {
    for owned in [32 << 10, 128 << 10] {
        let b = budget(owned, 1_000_000, Arc::new(AtomicBool::new(false)));
        let mut service = Service::new();
        drop(http(&mut service, request(&b), &b));
        let mut body = Vec::with_capacity(64 << 10);
        body.extend_from_slice(br#"{"data":[]}"#);
        let result = service.resume(HostReply::Http { status: 200, body }, &b);
        if owned == 32 << 10 { assert!(matches!(result, Err(Failure::Limit("owned-bytes")))); }
        else { assert!(matches!(result, Ok(Step::Done(_)))); }
        drop(result); drop(service);
        assert_eq!(b.owned_bytes(), Budget::context_bytes());
    }
}

#[test]
fn one_shot_and_unsupported_targets_never_yield_another_request() {
    let b = ample();
    for model in ["MiniLM", "voyage-context-3"] {
        assert!(matches!(RemoteTarget::capture(&text(model, &b), None, None, None, &b), Err(Failure::Unsupported("embedding-target"))));
    }
    let mut service = Service::new();
    assert!(matches!(service.resume(HostReply::Cancelled, &b), Err(Failure::Invalid("embedding-resume-state"))));
    assert!(matches!(service.run(request(&b), &b), Err(Failure::Invalid("embedding-run-state"))));
    let mut service = Service::new();
    let req = Request { target: RemoteTarget::Custom { endpoint: text("", &b) }, config: config(&b), purpose: Purpose::Query, input: Input::Scalar(text("q", &b)) };
    assert!(matches!(service.run(req, &b), Err(Failure::Invalid("embedding-custom-url"))));
    drop(service);
    assert_eq!(b.owned_bytes(), Budget::context_bytes());
}

#[test]
fn request_work_limit_and_pre_cancel_prevent_http_yield() {
    for (work, pre_cancel) in [(5000, false), (1_000_000, false), (1_000_000, true)] {
        let cancel = Arc::new(AtomicBool::new(false));
        let b = budget(1 << 20, work, cancel.clone());
        let mut req = request(&b);
        req.input = Input::Scalar(text(&"x".repeat(1024), &b));
        let mut service = Service::new();
        if pre_cancel { cancel.store(true, Ordering::Relaxed); }
        let result = service.run(req, &b);
        if pre_cancel { assert!(matches!(result, Err(Failure::Cancelled))); }
        else if work == 5000 { assert!(matches!(result, Err(Failure::Limit("work")))); }
        else { assert!(matches!(result, Ok(Step::NeedsHost(HostRequest::Http(_))))); }
        drop(result); drop(service);
        assert_eq!(b.owned_bytes(), Budget::context_bytes());
    }
}

#[test]
fn pending_control_and_wrong_replies_are_terminal() {
    for reply in [HostReply::Cancelled, HostReply::Unsupported("unavailable-synthetic-host".into()), HostReply::Value(risunest_ir::Value::Null)] {
        let b = ample();
        let mut service = Service::new();
        drop(http(&mut service, request(&b), &b));
        let expected = match &reply { HostReply::Cancelled => "cancelled", HostReply::Unsupported(_) => "unsupported", _ => "invalid" };
        let error = service.resume(reply, &b).unwrap_err();
        assert!(match (expected, &error) {
            ("cancelled", Failure::Cancelled) => true,
            ("unsupported", Failure::Unsupported("embedding-http-host")) => true,
            ("invalid", Failure::Invalid("embedding-http-reply")) => true,
            _ => false,
        });
        assert!(matches!(service.resume(HostReply::Cancelled, &b), Err(Failure::Invalid("embedding-resume-state"))));
        assert!(matches!(service.run(request(&b), &b), Err(Failure::Invalid("embedding-run-state"))));
        drop(error); drop(service);
        assert_eq!(b.owned_bytes(), Budget::context_bytes());
    }
}
