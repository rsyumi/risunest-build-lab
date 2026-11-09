use crate::chat::Role;
use crate::chat_ml::{parse_chat_ml, take_thoughts};
use crate::provider::{HttpMethod, HttpRequestSpec, ResponseKind};
use crate::stage::HostRequest;
use crate::{parse_json, parse_json_text, Budget, Error, Limits, Text, TextBuilder, Utf8Decoder, Value};
use std::sync::{atomic::{AtomicBool, Ordering}, Arc};

fn limits() -> Limits {
    Limits { max_owned_bytes: 1024 * 1024, max_json_bytes: 128 * 1024, max_text_units: 64 * 1024,
        max_values: 4096, max_depth: 64, max_work: 2 * 1024 * 1024 }
}
fn budget() -> Budget { Budget::new(limits(), Arc::new(AtomicBool::new(false))).unwrap() }
fn text(value: &Value) -> &Text { match value { Value::Text(value) => value, _ => panic!("Expected text") } }
fn utf16(text: &str) -> Vec<u16> { text.encode_utf16().collect() }
fn lossy(units: &[u16]) -> String { String::from_utf16_lossy(units) }
fn output(input: &str) -> String {
    let budget = budget();
    let value = parse_json(input, &budget).unwrap();
    value.to_text(&budget).unwrap().as_unicode().unwrap().to_owned()
}

#[test]
fn json_preserves_isolated_units_and_pairs_without_lossy_conversion() {
    let budget = budget();
    let isolated = parse_json(r#""\ud800""#, &budget).unwrap();
    assert_eq!(text(&isolated).units().collect::<Vec<_>>(), vec![0xd800]);
    assert!(text(&isolated).as_unicode().is_none());
    assert_eq!(text(&isolated).unicode_copy(&budget).unwrap_err(), Error::IsolatedUtf16);
    assert_eq!(text(&isolated).utf8_accounting_len(&budget).unwrap(), 3);
    let pair = parse_json(r#""\ud800\udc00""#, &budget).unwrap();
    assert_eq!(text(&pair).as_unicode(), Some("\u{10000}"));
    let mut joined = text(&isolated).copy(&budget).unwrap();
    joined.append(&Text::from_units(&[0xdc00], &budget).unwrap()).unwrap();
    assert_eq!(joined.as_unicode(), Some("\u{10000}"));
    assert_eq!(text(&isolated).units().collect::<Vec<_>>(), vec![0xd800]);
}

#[test]
fn utf8_decoder_handles_fragmented_bom_unicode_and_incomplete_eof() {
    let budget = budget();
    let mut decoder = Utf8Decoder::new();
    let mut decoded = Text::from_str("", &budget).unwrap();
    let bytes = [b"\xef\xbb\xbf".as_slice(), "합성🐿️".as_bytes()].concat();
    for byte in bytes { decoded.append(&decoder.decode(&[byte], false, &budget).unwrap()).unwrap(); }
    decoded.append(&decoder.decode(&[0xe2, 0x82], true, &budget).unwrap()).unwrap();
    assert_eq!(decoded.as_unicode(), Some("합성🐿️\u{fffd}"));
    assert_eq!(decoder.decode(&[], true, &budget).unwrap_err(), Error::DecoderFinished);
    let mut decoder = Utf8Decoder::new();
    assert_eq!(decoder.decode(b"\xff\xfe", true, &budget).unwrap().as_unicode(), Some("\u{fffd}\u{fffd}"));
}

#[test]
fn utf8_replacement_and_json_escape_surrogates_are_distinct() {
    let budget = budget();
    let mut decoder = Utf8Decoder::new();
    let decoded = decoder.decode(b"\"\xff\"", true, &budget).unwrap();
    let replaced = parse_json(decoded.as_unicode().unwrap(), &budget).unwrap();
    assert_eq!(text(&replaced).as_unicode(), Some("\u{fffd}"));
    let escaped = parse_json(r#""\udfff""#, &budget).unwrap();
    assert_eq!(text(&escaped).units().collect::<Vec<_>>(), vec![0xdfff]);
    assert!(text(&escaped).as_unicode().is_none());
}

#[test]
fn binary64_numbers_use_javascript_string_conventions() {
    assert_eq!(output("-0"), "0");
    assert_eq!(output("9007199254740993"), "9007199254740992");
    assert_eq!(output("1e21"), "1e+21");
    assert_eq!(output("1e1000"), "Infinity");
    assert_eq!(output("-1e1000"), "-Infinity");
    assert_eq!(output("1e-1000"), "0");
    let budget = budget();
    assert!(!parse_json("-0", &budget).unwrap().truthy());
    assert!(parse_json("1e1000", &budget).unwrap().truthy());
}

#[test]
fn json_array_and_object_coercion_uses_ordinary_json_value_semantics() {
    assert_eq!(output(r#"[1,null,{"x":1},[true,null],[]]"#), "1,,[object Object],true,,");
    assert_eq!(output(r#"{"valueOf":null}"#), "[object Object]");
    let budget = budget();
    assert!(parse_json("[]", &budget).unwrap().truthy());
    assert!(parse_json("{}", &budget).unwrap().truthy());
    let value = parse_json(r#"{"toString":null}"#, &budget).unwrap();
    assert_eq!(value.to_text(&budget).unwrap_err(), Error::Type { operation: "object-to-primitive" });
    let array = parse_json(r#"[{"toString":0}]"#, &budget).unwrap();
    assert_eq!(array.to_text(&budget).unwrap_err(), Error::Type { operation: "object-to-primitive" });
}

#[test]
fn public_error_values_are_not_replaced_by_their_message_text() {
    let budget = budget();
    let mut value = parse_json(r#"{"error":[1,null,{"x":true}],"status":500}"#, &budget).unwrap();
    let error = value.take(b"error", &budget).unwrap().unwrap();
    assert_eq!(error.to_text(&budget).unwrap().as_unicode(), Some("1,,[object Object]"));
    assert_eq!(error.array().unwrap().len(), 3);
    assert!(matches!(error.array().unwrap()[2].get(b"x", &budget).unwrap(), Some(Value::Bool(true))));
    assert!(value.get(b"error", &budget).unwrap().is_none());
}

#[test]
fn duplicate_keys_keep_last_value_and_proto_name_is_an_ordinary_key() {
    let budget = budget();
    let value = parse_json(r#"{"error":false,"error":42,"__proto__":{"error":"other"}}"#, &budget).unwrap();
    assert!(matches!(value.get(b"error", &budget).unwrap(), Some(Value::Number(number)) if *number == 42.0));
    assert_eq!(value.get(b"__proto__", &budget).unwrap().unwrap().get(b"error", &budget).unwrap().unwrap().to_text(&budget).unwrap().as_unicode(), Some("other"));
    let no_error = parse_json(r#"{"__proto__":{"error":"not inherited"}}"#, &budget).unwrap();
    assert!(no_error.get(b"error", &budget).unwrap().is_none());
}

#[test]
fn string_search_and_trim_preserve_javascript_units_and_whitespace() {
    let budget = budget();
    let value = Text::from_str("🐿 before Prompt alias: earlier system: later", &budget).unwrap();
    let first = value.index_of(&Text::from_str("Prompt alias:", &budget).unwrap(), &budget).unwrap().unwrap();
    let later = value.index_of(&Text::from_str("system:", &budget).unwrap(), &budget).unwrap().unwrap();
    assert!(first < later);
    assert_eq!(value.prefix(first, &budget).unwrap().trim(&budget).unwrap().as_unicode(), Some("🐿 before"));
    assert_eq!(Text::from_str("\u{feff} keep \u{85} ", &budget).unwrap().trim(&budget).unwrap().as_unicode(), Some("keep \u{85}"));
    assert_eq!(Text::from_str("🐿", &budget).unwrap().prefix(1, &budget).unwrap().units().collect::<Vec<_>>(), vec![0xd83d]);
}

#[test]
fn aggregate_owned_budget_counts_simultaneously_live_values_and_failed_growth() {
    let mut limits = limits();
    limits.max_owned_bytes = Budget::context_bytes() + 8;
    let budget = Budget::new(limits, Arc::new(AtomicBool::new(false))).unwrap();
    let mut first = Text::from_str("abcd", &budget).unwrap();
    let second = Text::from_str("efgh", &budget).unwrap();
    assert_eq!(budget.owned_bytes(), Budget::context_bytes() + 8);
    assert_eq!(first.append(&second).unwrap_err(), Error::Limit("owned-bytes"));
    assert_eq!(first.as_unicode(), Some("abcd"));
    assert_eq!(second.as_unicode(), Some("efgh"));
    assert_eq!(budget.owned_bytes(), Budget::context_bytes() + 8);
    assert!(budget.peak_bytes() <= limits.max_owned_bytes);
    drop(first);
    drop(second);
    assert_eq!(budget.owned_bytes(), Budget::context_bytes());
}

#[test]
fn syntax_resource_and_cancellation_failures_remain_distinct_and_release_values() {
    let budget = budget();
    assert!(matches!(parse_json(r#"{"unfinished":[1,]}"#, &budget), Err(Error::Syntax { .. })));
    assert_eq!(budget.owned_bytes(), Budget::context_bytes());
    let mut limits = limits();
    limits.max_values = 2;
    let limited = Budget::new(limits, Arc::new(AtomicBool::new(false))).unwrap();
    assert_eq!(parse_json("[1,2]", &limited).unwrap_err(), Error::Limit("json-values"));
    assert_eq!(limited.owned_bytes(), Budget::context_bytes());
    limits.max_depth = 0;
    let shallow = Budget::new(limits, Arc::new(AtomicBool::new(false))).unwrap();
    assert_eq!(parse_json("[0]", &shallow).unwrap_err(), Error::Limit("json-depth"));
    let cancel = Arc::new(AtomicBool::new(true));
    let cancelled = Budget::new(limits, cancel.clone()).unwrap();
    assert_eq!(parse_json("{}", &cancelled).unwrap_err(), Error::Cancelled);
    let mut decoder = Utf8Decoder::new();
    assert_eq!(decoder.decode(b"x", false, &cancelled).unwrap_err(), Error::Cancelled);
    cancel.store(false, Ordering::Relaxed);
    assert_eq!(decoder.decode(b"x", true, &cancelled).unwrap_err(), Error::DecoderFinished);
}

#[test]
fn json_grammar_and_depth_configuration_cannot_bypass_limits() {
    for invalid in ["01", "+1", "1.", "1e", "[true,]", "{1:2}", r#""\uZZZZ""#, "true false"] {
        assert!(matches!(parse_json(invalid, &budget()), Err(Error::Syntax { .. })), "{invalid}");
    }
    let mut limits = limits();
    limits.max_depth = 129;
    assert!(matches!(Budget::new(limits, Arc::new(AtomicBool::new(false))), Err(Error::Limit("invalid-limits"))));
}

#[test]
fn object_property_order_matches_javascript_integer_indices_then_insertion() {
    let budget = budget();
    let mut value = parse_json(r#"{"2":"old","a":1,"1":"one","0":"zero","2":"new","01":"ordinary","4294967295":"ordinary"}"#, &budget).unwrap();
    let keys = value.object().unwrap().iter().map(|(key, _)| key.as_unicode().unwrap()).collect::<Vec<_>>();
    assert_eq!(keys, ["0", "1", "2", "a", "01", "4294967295"]);
    value.take(b"0", &budget).unwrap().unwrap();
    assert_eq!(value.object().unwrap().iter().map(|(key, _)| key.as_unicode().unwrap()).collect::<Vec<_>>(), ["1", "2", "a", "01", "4294967295"]);
    assert_eq!(value.get(b"2", &budget).unwrap().unwrap().to_text(&budget).unwrap().as_unicode(), Some("new"));
}

#[test]
fn trim_charges_the_scanned_source_even_when_the_result_is_empty_or_one_unit() {
    let rich = budget();
    let spaces = Text::from_str(&" ".repeat(64), &rich).unwrap();
    let mut units = vec![32; 64];
    units.push(0xd800);
    let isolated = Text::from_units(&units, &rich).unwrap();
    let mut limits = limits();
    limits.max_work = 1;
    for value in [&spaces, &isolated] {
        let limited = Budget::new(limits, Arc::new(AtomicBool::new(false))).unwrap();
        assert_eq!(value.trim(&limited).unwrap_err(), Error::Limit("work"));
        assert_eq!(limited.owned_bytes(), Budget::context_bytes());
    }
}

#[test]
fn isolated_unit_upgrade_charges_the_existing_unicode_prefix_copy() {
    let mut limits = limits();
    limits.max_work = 48;
    let limited = Budget::new(limits, Arc::new(AtomicBool::new(false))).unwrap();
    let mut units = vec![97; 16];
    units.push(0xd800);
    let built = (|| {
        let mut builder = TextBuilder::new(&limited)?;
        for &unit in &units { builder.unit(unit)?; }
        builder.finish()
    })();
    assert_eq!(built.unwrap_err(), Error::Limit("work"));
    assert_eq!(limited.owned_bytes(), Budget::context_bytes());
}

#[test]
fn object_string_coercion_charges_own_property_lookup_work() {
    let rich = budget();
    let fields = (0..64).map(|index| format!("\"field{index}\":0")).collect::<Vec<_>>().join(",");
    let value = parse_json(&format!("{{{fields}}}"), &rich).unwrap();
    let mut limits = limits();
    limits.max_work = 32;
    let limited = Budget::new(limits, Arc::new(AtomicBool::new(false))).unwrap();
    assert_eq!(value.to_text(&limited).unwrap_err(), Error::Limit("work"));
    assert_eq!(limited.owned_bytes(), Budget::context_bytes());
}

#[test]
fn accessor_limits_are_errors_and_failed_removal_preserves_the_value() {
    let rich = budget();
    let mut value = parse_json(r#"{"x":0,"a":1,"b":2}"#, &rich).unwrap();
    let mut limits = limits();
    limits.max_work = 3;
    let limited = Budget::new(limits, Arc::new(AtomicBool::new(false))).unwrap();
    assert_eq!(value.take(b"x", &limited).unwrap_err(), Error::Limit("work"));
    assert!(matches!(value.get(b"x", &rich).unwrap(), Some(Value::Number(number)) if *number == 0.0));
    let limited = Budget::new(limits, Arc::new(AtomicBool::new(false))).unwrap();
    assert_eq!(value.get(b"absent", &limited).unwrap_err(), Error::Limit("work"));
    let isolated = Text::from_units(&[0xd800; 16], &rich).unwrap();
    let limited = Budget::new(limits, Arc::new(AtomicBool::new(false))).unwrap();
    assert_eq!(isolated.utf8_accounting_len(&limited).unwrap_err(), Error::Limit("work"));
}

#[test]
fn unicode_borrows_and_empty_prefix_are_constant_work_but_copy_and_search_are_charged() {
    let rich = budget();
    let source = Text::from_str(&"a".repeat(64), &rich).unwrap();
    let needle = Text::from_str("z", &rich).unwrap();
    let mut limits = limits();
    limits.max_work = 1;
    let limited = || Budget::new(limits, Arc::new(AtomicBool::new(false))).unwrap();
    assert_eq!(source.prefix(0, &limited()).unwrap().as_unicode(), Some(""));
    assert_eq!(source.utf8_accounting_len(&limited()).unwrap(), 64);
    assert_eq!(source.prefix(1, &limited()).unwrap_err(), Error::Limit("work"));
    assert_eq!(source.copy(&limited()).unwrap_err(), Error::Limit("work"));
    assert_eq!(source.index_of(&needle, &limited()).unwrap_err(), Error::Limit("work"));
    let empty = Text::from_str("", &rich).unwrap();
    assert_eq!(source.index_of(&empty, &limited()).unwrap(), Some(0));
    let longer = Text::from_str(&"z".repeat(65), &rich).unwrap();
    assert_eq!(source.index_of(&longer, &limited()).unwrap(), None);
}

#[test]
fn json_text_counts_an_isolated_unit_as_three_bytes_like_a_bmp_character() {
    let mut limits = limits();
    limits.max_json_bytes = 5;
    let exact = Budget::new(limits, Arc::new(AtomicBool::new(false))).unwrap();
    // `"`, U+D800, `"` is five WTF-8 bytes, as `"一"` is five UTF-8 bytes.
    let isolated = Text::from_units(&[34, 0xd800, 34], &exact).unwrap();
    assert_eq!(text(&parse_json_text(&isolated, &exact).unwrap()).units().collect::<Vec<_>>(), vec![0xd800]);
    assert!(parse_json("\"一\"", &exact).is_ok());
    limits.max_json_bytes = 4;
    let short = Budget::new(limits, Arc::new(AtomicBool::new(false))).unwrap();
    assert_eq!(parse_json_text(&Text::from_units(&[34, 0xd800, 34], &short).unwrap(), &short).unwrap_err(), Error::Limit("json-bytes"));
    assert_eq!(parse_json("\"一\"", &short).unwrap_err(), Error::Limit("json-bytes"));
    // The syntax error offset counts the same bytes, and the failed parse releases its WTF-8 copy.
    let budget = budget();
    let invalid = Text::from_units(&[91, 34, 0xd800, 34, 44, 120, 93], &budget).unwrap();
    let before = budget.owned_bytes();
    assert_eq!(parse_json_text(&invalid, &budget).unwrap_err(), Error::Syntax { byte: 7 });
    assert_eq!(parse_json("[\"一\",x]", &budget).unwrap_err(), Error::Syntax { byte: 7 });
    assert_eq!(budget.owned_bytes(), before);
}

#[test]
fn thoughts_run_from_the_first_opening_to_the_last_closing_tag() {
    let (rest, thought) = take_thoughts(&utf16("a<Thoughts>x</Thoughts>b<Thoughts>y</Thoughts>c"));
    assert_eq!((lossy(&rest), thought.map(|value| lossy(&value))), ("ac".into(), Some("x</Thoughts>b<Thoughts>y".into())));
    let (rest, thought) = take_thoughts(&utf16("<Thoughts></Thoughts>"));
    assert_eq!((lossy(&rest), thought), ("<Thoughts></Thoughts>".into(), None));
}

#[test]
fn chat_ml_reads_roles_separators_and_enders() {
    let budget = budget();
    let parts = parse_chat_ml(&utf16(" <|im_start|>system<|im_sep|>A<|im_end|><|im_start|>assistant\n B <|im_end|>\n<|im_start|>x"), &budget)
        .unwrap().unwrap();
    let view: Vec<_> = parts.iter().map(|part| (part.role, lossy(&part.content))).collect();
    assert_eq!(view, vec![(Role::System, "A".into()), (Role::Assistant, "B ".into()), (Role::User, "x".into())]);
    assert!(parse_chat_ml(&utf16("plain"), &budget).unwrap().is_none());
}

#[test]
fn http_request_debug_leaves_out_credentials_header_values_query_and_body() {
    let spec = HttpRequestSpec {
        method: HttpMethod::Post,
        url: "https://name:pass-word@api.example.test/v1/chat?key=query-key#fragment-key".to_owned(),
        headers: vec![("Authorization".to_owned(), "Bearer header-key".to_owned()), ("Content-Type".to_owned(), "application/json".to_owned())],
        body: br#"{"prompt":"body-text"}"#.to_vec(),
        response: ResponseKind::Sse,
    };
    let body = format!("{:?}", spec.body);
    let mut noncanonical = spec.clone();
    noncanonical.url = "https:////user:secret@host/".to_owned();
    for shown in [format!("{noncanonical:?}"), format!("{:?}", HostRequest::Http(noncanonical.clone()))] {
        assert!(!shown.contains("user:secret"), "raw endpoint credential in {shown}");
    }
    // A stage's own call reaches the owner as `HostRequest::Http`, whose `Debug` shows the spec the same way.
    for shown in [format!("{spec:?}"), format!("{:?}", HostRequest::Http(spec.clone()))] {
        for hidden in ["pass-word", "name:", "api.example.test", "/v1/chat", "query-key", "fragment-key", "header-key", "application/json", "body-text", body.as_str()] {
            assert!(!shown.contains(hidden), "{hidden} in {shown}");
        }
        for kept in ["Post", "url: \"<redacted>\"", "\"Authorization\"", "\"Content-Type\"", "body_bytes: 22", "Sse"] {
            assert!(shown.contains(kept), "{kept} not in {shown}");
        }
    }
}
