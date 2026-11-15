//! Contracts of the `ir` bodies that the Node oracle sets cannot express: refusals, limits and entropy.

use risunest_ir::chat::{AssetRef, Multimodal, MultimodalData, MultimodalKind, PromptMessage, Role};
use risunest_ir::rng::{OsEntropy, Rng};
use risunest_ir::{json, num, parse_json, Budget, Error, Limits, Text, Value};
use std::sync::{atomic::AtomicBool, Arc};

fn limits() -> Limits {
    Limits { max_owned_bytes: 1 << 20, max_json_bytes: 128 << 10, max_text_units: 64 << 10, max_values: 4096, max_depth: 64, max_work: 2 << 20 }
}
fn budget() -> Budget { Budget::new(limits(), Arc::new(AtomicBool::new(false))).unwrap() }
fn limited(change: impl FnOnce(&mut Limits)) -> Budget {
    let mut limits = limits();
    change(&mut limits);
    Budget::new(limits, Arc::new(AtomicBool::new(false))).unwrap()
}
fn stringify(value: &Value, budget: &Budget) -> String { json::stringify(value, budget).unwrap().unwrap().as_unicode().unwrap().to_owned() }

#[test]
fn os_entropy_draws_distinct_multiples_of_two_to_the_minus_53_below_one() {
    let mut rng = OsEntropy;
    let values = (0..64).map(|_| rng.random().unwrap()).collect::<Vec<_>>();
    for value in &values {
        assert!((0.0..1.0).contains(value), "{value}");
        assert_eq!((value * 9_007_199_254_740_992.0).fract(), 0.0, "{value}");
    }
    let mut sorted = values.clone();
    sorted.sort_by(f64::total_cmp);
    sorted.dedup();
    assert!(sorted.len() > 60, "{values:?}");
}

#[test]
fn prompt_message_refuses_shapes_that_have_no_openai_chat_form() {
    let budget = budget();
    let refused = |source: &str| PromptMessage::from_value(parse_json(source, &budget).unwrap(), &budget).unwrap_err();
    assert_eq!(refused(r#"[]"#), Error::Type { operation: "prompt-message" });
    assert_eq!(refused(r#"{"content":"x"}"#), Error::Type { operation: "prompt-message-role" });
    assert_eq!(refused(r#"{"role":"model","content":"x"}"#), Error::Type { operation: "prompt-message-role" });
    assert_eq!(refused(r#"{"role":"user"}"#), Error::Type { operation: "prompt-message-content" });
    assert_eq!(refused(r#"{"role":"user","content":1}"#), Error::Type { operation: "prompt-message-content" });
    assert_eq!(refused(r#"{"role":"user","content":"x","memo":null}"#), Error::Type { operation: "prompt-message-memo" });
    assert_eq!(refused(r#"{"role":"user","content":"x","attr":["a",1]}"#), Error::Type { operation: "prompt-message-attr" });
    assert_eq!(refused(r#"{"role":"user","content":"x","removable":"yes"}"#), Error::Type { operation: "prompt-message-removable" });
    assert_eq!(refused(r#"{"role":"user","content":"x","multimodals":{}}"#), Error::Type { operation: "prompt-message-multimodals" });
    assert_eq!(refused(r#"{"role":"user","content":"x","multimodals":[{"type":"image"}]}"#), Error::Type { operation: "multimodal-base64" });
    assert_eq!(refused(r#"{"role":"user","content":"x","multimodals":[{"type":"gif","base64":""}]}"#), Error::Type { operation: "multimodal-type" });
    assert_eq!(refused(r#"{"role":"user","content":"x","multimodals":[{"type":"image","base64":"","width":"1"}]}"#), Error::Type { operation: "multimodal-width" });
}

#[test]
fn unresolved_asset_and_hand_built_duplicates_follow_the_javascript_object_model() {
    let budget = budget();
    let asset = Multimodal { kind: MultimodalKind::Image, data: MultimodalData::Asset(AssetRef(Text::from_str("a", &budget).unwrap())),
        width: None, height: None, extra: Vec::new() };
    assert_eq!(asset.to_value(&budget).unwrap_err(), Error::Invariant("multimodal-asset-unresolved"));
    let mut message = PromptMessage::new(Role::User, Text::from_str("x", &budget).unwrap());
    message.multimodals = Some(vec![asset]);
    assert_eq!(message.to_value(&budget).unwrap_err(), Error::Invariant("multimodal-asset-unresolved"));
    // An extra property named like a field replaces it in place, as Object.assign would.
    let mut message = PromptMessage::new(Role::User, Text::from_str("x", &budget).unwrap());
    message.extra.push((Text::from_str("role", &budget).unwrap(), Value::Null));
    message.extra.push((Text::from_str("3", &budget).unwrap(), Value::Bool(true)));
    assert_eq!(stringify(&message.to_value(&budget).unwrap(), &budget), r#"{"3":true,"role":null,"content":"x"}"#);
}

#[test]
fn stringify_limits_fail_without_leaking_owned_bytes() {
    let rich = budget();
    let deep = parse_json(&format!("{}0{}", "[".repeat(10), "]".repeat(10)), &rich).unwrap();
    let shallow = limited(|limits| limits.max_depth = 4);
    assert_eq!(json::stringify(&deep, &shallow).unwrap_err(), Error::Limit("json-depth"));
    assert_eq!(shallow.owned_bytes(), Budget::context_bytes());
    let escaped = parse_json(r#""\u0001\u0001\u0001""#, &rich).unwrap();
    let narrow = limited(|limits| limits.max_text_units = 10);
    assert_eq!(json::stringify(&escaped, &narrow).unwrap_err(), Error::Limit("text-units"));
    assert_eq!(narrow.owned_bytes(), Budget::context_bytes());
    let output = json::stringify(&escaped, &rich).unwrap().unwrap();
    assert_eq!(output.len_units(), 20);
    assert_eq!(output.units().count(), 20);
}

#[test]
fn edge_trims_and_number_conversion_charge_the_scanned_source() {
    let rich = budget();
    let spaces = Text::from_str(&" ".repeat(64), &rich).unwrap();
    let mut units = vec![32; 64];
    units.push(0xd800);
    let isolated = Text::from_units(&units, &rich).unwrap();
    for value in [&spaces, &isolated] {
        for result in [value.trim_start(&limited(|limits| limits.max_work = 1)), value.trim_end(&limited(|limits| limits.max_work = 1))] {
            assert_eq!(result.unwrap_err(), Error::Limit("work"));
        }
        assert_eq!(num::to_number(value, &limited(|limits| limits.max_work = 1)).unwrap_err(), Error::Limit("work"));
    }
    assert_eq!(isolated.trim_start(&rich).unwrap().units().collect::<Vec<_>>(), vec![0xd800]);
    assert_eq!(isolated.trim_end(&rich).unwrap().len_units(), 65);
}

#[test]
fn from_units_charges_its_length_once_and_keeps_the_storage_and_limits() {
    let rich = budget();
    let pair = "a\u{1f600}\u{d55c}b".encode_utf16().collect::<Vec<_>>();
    let inputs: [(&[u16], bool); 7] = [
        (&[0x68, 0x65, 0x6c, 0x6c, 0x6f], true),
        (&pair, true),
        (&[0x61, 0xd800, 0x62, 0x63], false),
        (&[0xdc00, 0x61, 0x62, 0x63], false),
        (&[0x61, 0x62, 0x63, 0xd83d], false),
        (&[0x61, 0xdc00, 0x62], false),
        (&[0xd800, 0xd800, 0xdc00], false),
    ];
    for (units, well_formed) in inputs {
        let text = Text::from_units(units, &rich).unwrap();
        assert_eq!(text.units().collect::<Vec<_>>(), units);
        assert_eq!((text.len_units(), text.as_unicode().is_some()), (units.len(), well_formed), "{units:x?}");
        let exact = limited(|limits| limits.max_work = units.len());
        assert_eq!(Text::from_units(units, &exact).unwrap().units().collect::<Vec<_>>(), units);
        for (budget, name) in [
            (limited(|limits| limits.max_work = units.len() - 1), "work"),
            (limited(|limits| limits.max_text_units = units.len() - 1), "text-units"),
            (limited(|limits| limits.max_owned_bytes = Budget::context_bytes() + 1), "owned-bytes"),
        ] {
            assert_eq!(Text::from_units(units, &budget).unwrap_err(), Error::Limit(name), "{units:x?}");
            assert_eq!(budget.owned_bytes(), Budget::context_bytes());
        }
    }
    // The UTF-8 reservation is exact at each width edge: 64 bytes charge 64, and 65 charge 128 rather than growing 64.
    let edges = [0x7f, 0x80, 0x7ff, 0x800, 0xd7ff, 0xe000, 0xffff, 0xd83d, 0xde00];
    for length in [64, 65] {
        let mut units = edges.to_vec();
        units.resize(length - 21 + edges.len(), 0x61);
        let before = rich.owned_bytes();
        let text = Text::from_units(&units, &rich).unwrap();
        assert_eq!(text.as_unicode().map(str::len), Some(length));
        assert_eq!(rich.owned_bytes() - before, length.next_power_of_two());
    }
}

#[test]
fn units_know_how_many_remain_under_either_storage() {
    let budget = budget();
    let unicode = Text::from_str("a\u{1f600}\u{d55c}b", &budget).unwrap();
    let isolated = Text::from_units(&[0x61, 0xd800, 0x62, 0x63, 0x64], &budget).unwrap();
    assert!(unicode.as_unicode().is_some() && isolated.as_unicode().is_none());
    for text in [&unicode, &isolated] {
        let all = text.units().collect::<Vec<_>>();
        let mut units = text.units();
        for taken in 0..=all.len() {
            let left = all.len() - taken;
            assert_eq!((units.len(), units.size_hint()), (left, (left, Some(left))));
            assert_eq!(units.clone().count(), left);
            assert_eq!(units.clone().fold(Vec::new(), |mut rest, unit| { rest.push(unit); rest }), all[taken..]);
            units.next();
        }
        assert_eq!(units.next(), None);
    }
}

// Debug builds check the order `Value::Object` documents; JSON.parse and the chat conversions already keep it.
#[cfg(debug_assertions)]
#[test]
#[should_panic(expected = "JS property order")]
fn stringify_rejects_a_hand_built_object_out_of_javascript_property_order() {
    let budget = budget();
    assert_eq!(stringify(&parse_json(r#"{"b":1,"2":2,"1":3}"#, &budget).unwrap(), &budget), r#"{"1":3,"2":2,"b":1}"#);
    let object = |keys: &[&str]| {
        let mut entries = risunest_ir::OwnedVec::new(&budget).unwrap();
        for key in keys { entries.push((Text::from_str(key, &budget).unwrap(), Value::Null)).unwrap(); }
        Value::Object(entries)
    };
    assert_eq!(stringify(&object(&["1", "4294967295", "b"]), &budget), r#"{"1":null,"4294967295":null,"b":null}"#);
    stringify(&object(&["b", "1"]), &budget);
}

#[test]
fn define_keeps_javascript_property_order_and_replaces_existing_keys_in_place() {
    let budget = budget();
    let key = |name: &str| Text::from_str(name, &budget).unwrap();
    let mut object = Value::Object(risunest_ir::OwnedVec::new(&budget).unwrap());
    let pairs = [("b", 1.0), ("2", 2.0), ("a", 3.0), ("10", 4.0), ("1", 5.0), ("4294967295", 6.0), ("01", 7.0), ("__proto__", 8.0),
        ("b", 9.0), ("10", 10.0)];
    for (name, value) in pairs { object.define(key(name), Value::Number(value), &budget).unwrap(); }
    // What JS holds after defining each pair in turn as an enumerable data property.
    let expected = r#"{"1":5,"2":2,"10":10,"b":9,"a":3,"4294967295":6,"01":7,"__proto__":8}"#;
    assert_eq!(stringify(&object, &budget), expected);

    let narrow = limited(|limits| limits.max_work = 3);
    assert_eq!(object.define(key("0"), Value::Null, &narrow).unwrap_err(), Error::Limit("work"));
    assert_eq!(stringify(&object, &budget), expected);
    for mut other in [Value::Null, parse_json("[]", &budget).unwrap()] {
        assert_eq!(other.define(key("0"), Value::Null, &budget).unwrap_err(), Error::Type { operation: "define-property" });
    }
}
