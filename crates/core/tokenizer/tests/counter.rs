//! `tokenizeChat` accounting and the plugin/Google tokenizer host protocols of [`Counter`]. The Google Cloud tests need the
//! Gemma fallback, which only a build with `huggingface` runs.

mod common;

use risunest_ir::chat::{Multimodal, MultimodalData, MultimodalKind, PromptMessage, Role};
use risunest_ir::stage::{HostReply, HostRequest, StageError, Step, TokenCounter, TokenInput};
use risunest_ir::{Budget, Limits, Text};
use risunest_tokenizer::counter::{ChatAccounting, Counter, PluginMemoLimits};
#[cfg(feature = "huggingface")]
use common::ready;
#[cfg(feature = "huggingface")]
use risunest_tokenizer::counter::encode::Encoder;
#[cfg(feature = "huggingface")]
use risunest_tokenizer::counter::google::{GoogleCloud, GoogleCloudMemo};
use risunest_tokenizer::id::TokenizerId;
use std::sync::{atomic::{AtomicBool, Ordering}, Arc};

fn limits() -> Limits {
    Limits { max_owned_bytes: 16 << 20, max_json_bytes: 1 << 20, max_text_units: 1 << 20, max_values: 1 << 16, max_depth: 64,
        max_work: 1 << 32 }
}
fn budget() -> Budget { Budget::new(limits(), Arc::new(AtomicBool::new(false))).unwrap() }
fn text(value: &str, budget: &Budget) -> Text { Text::from_str(value, budget).unwrap() }
fn message(content: &str, name: Option<&str>, budget: &Budget) -> PromptMessage {
    let mut message = PromptMessage::new(Role::User, text(content, budget));
    message.name = name.map(|name| text(name, budget));
    message
}
fn image(width: Option<f64>, height: Option<f64>, budget: &Budget) -> Multimodal {
    Multimodal { kind: MultimodalKind::Image, data: MultimodalData::Inline(text("data:,", budget)), width, height, extra: Vec::new() }
}
fn done(step: Step<u32>) -> u32 { match step { Step::Done(count) => count, Step::NeedsHost(request) => panic!("unexpected host request {request:?}") } }
fn requested(step: Step<u32>) -> String {
    match step {
        Step::NeedsHost(HostRequest::PluginTokenizer { text }) => text.as_unicode().unwrap().to_owned(),
        step => panic!("expected a plugin tokenizer request, got {step:?}"),
    }
}

// Expected counts follow `ChatTokenizer.tokenizeChat` and `tokenizeMultiModal` in src/ts/tokenizer.ts; "hello" is one
// cl100k token (the corpus pins its ID).
#[test]
fn built_in_message_accounting_adds_message_name_and_multimodal_tokens() {
    let budget = budget();
    let accounting = ChatAccounting { additional_tokens: 3, use_name: true, inlay_images: true, low_vision_quality: false };
    let named = message("hello", Some("hello"), &budget);
    let count = |accounting: ChatAccounting, input: TokenInput<'_>| done(Counter::new(TokenizerId::Cl100kBase, accounting).count(input, &budget).unwrap());
    assert_eq!(count(accounting, TokenInput::Message(&named)), 1 + 3 + 1 + 1);
    assert_eq!(count(ChatAccounting { use_name: false, ..accounting }, TokenInput::Message(&named)), 1 + 3);
    assert_eq!(count(accounting, TokenInput::Message(&message("hello", Some(""), &budget))), 1 + 3);
    assert_eq!(count(accounting, TokenInput::Text(&named.content)), 1);

    let mut pictured = message("hello", None, &budget);
    pictured.multimodals = Some(vec![
        image(Some(1024.0), Some(1024.0), &budget), // square, clamped to 768: 2 x 2 chunks
        image(Some(2000.0), Some(1000.0), &budget), // only the height is clamped: 4 x 2 chunks
        image(Some(1000.0), Some(2000.0), &budget), // only the width is clamped: 2 x 4 chunks
        image(None, None, &budget),                 // no dimensions: no chunks
    ]);
    assert_eq!(count(accounting, TokenInput::Message(&pictured)), 1 + 3 + (3 + 8 + 85) + (3 + 16 + 85) * 2 + (3 + 85));
    assert_eq!(count(ChatAccounting { low_vision_quality: true, ..accounting }, TokenInput::Message(&pictured)), 1 + 3 + 87 * 4);
    assert_eq!(count(ChatAccounting { inlay_images: false, ..accounting }, TokenInput::Message(&pictured)), 1 + 3 + 3 * 4);
    let messages = [named, pictured];
    assert_eq!(count(accounting, TokenInput::Messages(&messages)), 6 + 1 + 3 + 96 + 104 * 2 + 88);
}

#[test]
fn plugin_tokenizer_asks_the_host_for_each_text_in_typescript_order() {
    let budget = budget();
    let accounting = ChatAccounting { additional_tokens: 2, use_name: true, ..ChatAccounting::default() };
    let messages = [message("a", Some("n"), &budget), message("b", Some(""), &budget)];
    let mut counter = Counter::new(TokenizerId::Plugin, accounting);
    assert_eq!(requested(counter.count(TokenInput::Messages(&messages), &budget).unwrap()), "a");
    assert_eq!(counter.count(TokenInput::Text(&messages[0].content), &budget).unwrap_err(), StageError::Invalid("token-counter-busy".into()));
    assert_eq!(requested(counter.resume(HostReply::Tokens(vec![1.0, 2.0]), &budget).unwrap()), "n");
    assert_eq!(requested(counter.resume(HostReply::Tokens(vec![5.0]), &budget).unwrap()), "b");
    assert_eq!(done(counter.resume(HostReply::Tokens(Vec::new()), &budget).unwrap()), (2 + 2) + (1 + 1) + 2);
    assert_eq!(counter.resume(HostReply::Tokens(vec![1.0]), &budget).unwrap_err(), StageError::Invalid("token-counter-resume".into()));
}

#[test]
fn plugin_memo_reuses_results_like_the_typescript_encode_cache() {
    let budget = budget();
    let messages = [message("same", None, &budget), message("same", None, &budget), message("other", None, &budget)];
    let requests = |mut counter: Counter| {
        let mut seen = Vec::new();
        let mut step = counter.count(TokenInput::Messages(&messages), &budget).unwrap();
        loop {
            match step {
                Step::Done(count) => return (seen, count),
                step => seen.push(requested(step)),
            }
            step = counter.resume(HostReply::Tokens(vec![0.0; 3]), &budget).unwrap();
        }
    };
    assert_eq!(requests(Counter::new(TokenizerId::Plugin, ChatAccounting::default()).with_plugin_memo()), (vec!["same".into(), "other".into()], 9));
    assert_eq!(requests(Counter::new(TokenizerId::Plugin, ChatAccounting::default())), (vec!["same".into(), "same".into(), "other".into()], 9));
}

#[test]
fn plugin_memo_keeps_the_most_recently_used_texts_within_its_bounds() {
    let budget = budget();
    // Counts one text and says whether it reached the host.
    let asked = |counter: &mut Counter, value: &str| {
        let input = text(value, &budget);
        match counter.count(TokenInput::Text(&input), &budget).unwrap() {
            Step::Done(_) => false,
            step => {
                assert_eq!(requested(step), value);
                done(counter.resume(HostReply::Tokens(vec![0.0]), &budget).unwrap());
                true
            }
        }
    };
    let memo = |entries, max_total_units| Counter::new(TokenizerId::Plugin, ChatAccounting::default())
        .with_plugin_memo_limits(PluginMemoLimits { entries, max_text_units: 4, max_total_units });

    let mut counter = memo(2, 64);
    assert!(asked(&mut counter, "aa") && asked(&mut counter, "bb"));
    assert!(!asked(&mut counter, "aa"));
    // Full: "bb" is the least recently used entry, so it goes and the reused "aa" stays.
    assert!(asked(&mut counter, "cc"));
    assert!(!asked(&mut counter, "aa"));
    assert!(asked(&mut counter, "bb"));
    // A text over max_text_units is never kept.
    assert!(asked(&mut counter, "eeeee") && asked(&mut counter, "eeeee"));
    assert!(!asked(&mut counter, "aa") && !asked(&mut counter, "bb"));

    // The total units bound evicts by recency too.
    let mut counter = memo(16, 6);
    assert!(asked(&mut counter, "aaa") && asked(&mut counter, "bbb"));
    assert!(!asked(&mut counter, "aaa"));
    assert!(asked(&mut counter, "cc"));
    assert!(!asked(&mut counter, "aaa") && !asked(&mut counter, "cc"));
    assert!(asked(&mut counter, "bbb"));
}

// Owners move stage objects between threads.
const _: fn() = || { fn send<T: Send>() {} send::<Counter>(); };

#[test]
fn plugin_host_failures_end_the_count_and_free_the_counter() {
    let budget = budget();
    let input = text("x", &budget);
    // The message reaches the caller unit for unit, isolated surrogate included.
    let mut thrown = "thrown \u{e9} ".encode_utf16().collect::<Vec<_>>();
    thrown.push(0xd800);
    let mut counter = Counter::new(TokenizerId::Plugin, ChatAccounting::default());
    for (reply, error) in [
        (HostReply::Failed { message: Text::from_units(&thrown, &budget).unwrap() }, StageError::Host(thrown.clone())),
        (HostReply::Unsupported("plugin-tokenizer".into()), StageError::Unsupported("plugin-tokenizer".into())),
        (HostReply::Cancelled, StageError::Cancelled),
        (HostReply::Embeddings(Vec::new()), StageError::Invalid("plugin-tokenizer-reply".into())),
    ] {
        assert_eq!(requested(counter.count(TokenInput::Text(&input), &budget).unwrap()), "x");
        assert_eq!(counter.resume(reply, &budget).unwrap_err(), error);
    }
    assert_eq!(requested(counter.count(TokenInput::Text(&input), &budget).unwrap()), "x");
    assert_eq!(done(counter.resume(HostReply::Tokens(vec![7.0]), &budget).unwrap()), 1);
}

#[test]
fn budget_failures_and_cancellation_mid_count_leave_the_counter_free() {
    let rich = budget();
    let input = text(&"x".repeat(64), &rich);
    let mut counter = Counter::new(TokenizerId::Plugin, ChatAccounting::default());
    let free = |counter: &mut Counter| {
        assert_eq!(requested(counter.count(TokenInput::Text(&input), &rich).unwrap()).len(), 64);
        assert_eq!(done(counter.resume(HostReply::Tokens(vec![0.0; 2]), &rich).unwrap()), 2);
    };

    // The work limit admits the queued copy of the text but not the request's copy.
    let mut limits = limits();
    limits.max_work = 192;
    let limited = Budget::new(limits, Arc::new(AtomicBool::new(false))).unwrap();
    assert_eq!(counter.count(TokenInput::Text(&input), &limited).unwrap_err(), StageError::Limit("work".into()));
    free(&mut counter);

    let cancel = Arc::new(AtomicBool::new(false));
    let cancellable = Budget::new(self::limits(), cancel.clone()).unwrap();
    assert_eq!(requested(counter.count(TokenInput::Text(&input), &cancellable).unwrap()).len(), 64);
    cancel.store(true, Ordering::Relaxed);
    assert_eq!(counter.resume(HostReply::Tokens(vec![0.0]), &cancellable).unwrap_err(), StageError::Cancelled);
    assert_eq!(counter.resume(HostReply::Tokens(vec![0.0]), &rich).unwrap_err(), StageError::Invalid("token-counter-resume".into()));
    free(&mut counter);
}

#[cfg(feature = "huggingface")]
fn google(memo: &GoogleCloudMemo, budget: &Budget) -> GoogleCloud {
    GoogleCloud { internal_id: text("synthetic-gemma-fallback", budget), access_token: None, memo: memo.clone() }
}
#[cfg(feature = "huggingface")]
fn http(status: u16, body: &[u8]) -> HostReply { HostReply::Http { status, body: body.to_vec() } }
#[cfg(feature = "huggingface")]
fn google_requested<T: std::fmt::Debug>(step: Step<T>, expected: &str) {
    let Step::NeedsHost(HostRequest::Http(request)) = step else { panic!("expected Google HTTP request, got {step:?}"); };
    let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
    assert_eq!(body["contents"][0]["parts"][0]["text"], expected);
}
#[cfg(feature = "huggingface")]
fn ids(step: Step<Vec<f64>>) -> Vec<f64> {
    match step { Step::Done(ids) => ids, Step::NeedsHost(request) => panic!("unexpected host request {request:?}") }
}

#[cfg(feature = "huggingface")]
#[test]
fn google_fallback_counts_each_segment_once_and_caches_only_parsed_200_answers() {
    let budget = budget();
    let memo = GoogleCloudMemo::new();
    let input = "<|turn>fallback";
    let name = "API name";
    let input_text = text(input, &budget);
    let expected = ids(Encoder::new(ready(TokenizerId::Gemma)).encode(&input_text, &budget).unwrap());
    assert!(expected.iter().any(|id| *id != 0.0));
    let mut named = message(input, Some(name), &budget);
    named.multimodals = Some(vec![image(Some(640.0), Some(480.0), &budget)]);
    let accounting = ChatAccounting { additional_tokens: 3, use_name: true, inlay_images: true, low_vision_quality: false };
    let mut counter = Counter::new(TokenizerId::GoogleCloud, accounting).with_google_cloud(google(&memo, &budget));
    google_requested(counter.count(TokenInput::Message(&named), &budget).unwrap(), input);
    google_requested(counter.resume(http(503, &[0xff, b'{']), &budget).unwrap(), name);
    let count = expected.len() as u32 + 5 + 1 + 3 + (3 + 4 + 85);
    assert_eq!(done(counter.resume(http(200, br#"{"totalTokens":5}"#), &budget).unwrap()), count);
    // The fallback content is requested again; the API answer for the name is reused.
    google_requested(counter.count(TokenInput::Message(&named), &budget).unwrap(), input);
    assert_eq!(done(counter.resume(http(500, b"not JSON"), &budget).unwrap()), count);
    let mut encoder = Encoder::new(TokenizerId::GoogleCloud).with_google_cloud(google(&memo, &budget));
    assert_eq!(ids(encoder.encode(&text(name, &budget), &budget).unwrap()), vec![0.0; 5]);
    google_requested(encoder.encode(&input_text, &budget).unwrap(), input);
    assert_eq!(ids(encoder.resume(http(404, b"ignored"), &budget).unwrap()), expected);
    google_requested(encoder.encode(&input_text, &budget).unwrap(), input);
    assert_eq!(encoder.resume(http(200, br#"{"totalTokens":-1}"#), &budget).unwrap_err(),
        StageError::Invalid("google-cloud-token-length".into()));
    // Parsed 200 is memoized before its invalid length fails, exactly like the TS array constructor.
    assert_eq!(encoder.encode(&input_text, &budget).unwrap_err(), StageError::Invalid("google-cloud-token-length".into()));
    assert_eq!(counter.count(TokenInput::Text(&input_text), &budget).unwrap_err(), StageError::Invalid("google-cloud-token-length".into()));
}

#[cfg(feature = "huggingface")]
#[test]
fn google_fallback_budget_and_host_failures_end_pending_operations_and_allow_retry() {
    let rich = budget();
    let input = text("retry <|turn>", &rich);
    let expected = ids(Encoder::new(ready(TokenizerId::Gemma)).encode(&input, &rich).unwrap());
    for mode in 0..3 {
        let mut constrained = limits();
        if mode == 0 { constrained.max_work = 1; }
        if mode == 1 { constrained.max_owned_bytes = Budget::context_bytes(); }
        let constrained = Budget::new(constrained, Arc::new(AtomicBool::new(mode == 2))).unwrap();
        let error = match mode { 0 => StageError::Limit("work".into()), 1 => StageError::Limit("owned-bytes".into()), _ => StageError::Cancelled };
        let memo = GoogleCloudMemo::new();
        let mut encoder = Encoder::new(TokenizerId::GoogleCloud).with_google_cloud(google(&memo, &rich));
        google_requested(encoder.encode(&input, &rich).unwrap(), "retry <|turn>");
        assert_eq!(encoder.resume(http(503, b"ignored"), &constrained).unwrap_err(), error);
        assert_eq!(encoder.resume(http(503, b"ignored"), &rich).unwrap_err(), StageError::Invalid("token-encoder-resume".into()));
        google_requested(encoder.encode(&input, &rich).unwrap(), "retry <|turn>");
        assert_eq!(ids(encoder.resume(http(503, b"ignored"), &rich).unwrap()), expected);
        let named = message("retry <|turn>", Some("API retry name"), &rich);
        let accounting = ChatAccounting { additional_tokens: 3, use_name: true, ..ChatAccounting::default() };
        let mut counter = Counter::new(TokenizerId::GoogleCloud, accounting).with_google_cloud(google(&memo, &rich));
        google_requested(counter.count(TokenInput::Message(&named), &rich).unwrap(), "retry <|turn>");
        assert_eq!(counter.resume(http(503, b"ignored"), &constrained).unwrap_err(), error);
        assert_eq!(counter.resume(http(503, b"ignored"), &rich).unwrap_err(), StageError::Invalid("token-counter-resume".into()));
        google_requested(counter.count(TokenInput::Message(&named), &rich).unwrap(), "retry <|turn>");
        google_requested(counter.resume(http(503, b"ignored"), &rich).unwrap(), "API retry name");
        assert_eq!(done(counter.resume(http(200, br#"{"totalTokens":5}"#), &rich).unwrap()), expected.len() as u32 + 5 + 1 + 3);
    }
    for mode in 0..3 {
        let reply = || match mode {
            0 => HostReply::Failed { message: text("synthetic fetch failure", &rich) },
            1 => HostReply::Cancelled,
            _ => HostReply::Embeddings(Vec::new()),
        };
        let error = match mode {
            0 => StageError::Host("synthetic fetch failure".encode_utf16().collect()),
            1 => StageError::Cancelled,
            _ => StageError::Invalid("google-cloud-reply".into()),
        };
        let memo = GoogleCloudMemo::new();
        let mut encoder = Encoder::new(TokenizerId::GoogleCloud).with_google_cloud(google(&memo, &rich));
        google_requested(encoder.encode(&input, &rich).unwrap(), "retry <|turn>");
        assert_eq!(encoder.resume(reply(), &rich).unwrap_err(), error);
        google_requested(encoder.encode(&input, &rich).unwrap(), "retry <|turn>");
        assert_eq!(ids(encoder.resume(http(503, b"ignored"), &rich).unwrap()), expected);
        let mut counter = Counter::new(TokenizerId::GoogleCloud, ChatAccounting::default()).with_google_cloud(google(&memo, &rich));
        google_requested(counter.count(TokenInput::Text(&input), &rich).unwrap(), "retry <|turn>");
        assert_eq!(counter.resume(reply(), &rich).unwrap_err(), error);
        google_requested(counter.count(TokenInput::Text(&input), &rich).unwrap(), "retry <|turn>");
        assert_eq!(done(counter.resume(http(503, b"ignored"), &rich).unwrap()), expected.len() as u32);
    }
}
