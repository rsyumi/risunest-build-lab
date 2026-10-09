use risunest_sync_wire::{canonical, order::{read_order, LiveRecord}, payload_value, stamp::*, unit::*, WireError};
use serde_json::json;
use std::collections::BTreeSet;

const WRITER: &str = "00000000-0000-4000-8000-000000000001";
fn stamp(physical: u64, logical: u32) -> Stamp { Stamp { physical_ms: physical.into(), logical, writer_id: WRITER.into() } }

#[test]
fn stamp_encoding_is_number_free_and_range_checked() {
    let stamp = stamp(u64::MAX, u32::MAX);
    let bytes = canonical::encode(&stamp).unwrap();
    assert_eq!(String::from_utf8(bytes.clone()).unwrap(), format!("{{\"logical\":\"4294967295\",\"physicalMs\":\"18446744073709551615\",\"writerId\":\"{WRITER}\"}}"));
    assert_eq!(canonical::decode::<Stamp>(&bytes, 1024).unwrap(), stamp);
    for value in ["", "01", "-1", "+1", " 1", "18446744073709551616"] { assert!(DecimalU64::try_from(value.to_owned()).is_err()); }
    for value in [json!({"physicalMs":"0","logical":"4294967296","writerId":WRITER}), json!({"physicalMs":"0","logical":"0","writerId":"bad"}), json!({"physicalMs":"00","logical":"0","writerId":WRITER}), json!({"physicalMs":0,"logical":"0","writerId":WRITER})] {
        assert!(serde_json::from_value::<Stamp>(value).is_err());
    }
}
#[test]
fn hlc_handles_wall_jumps_restart_observation_and_checked_overflow() {
    let last = stamp(100, 5);
    assert_eq!(issue_stamp(90, WRITER, Some(&last), None).unwrap(), stamp(100, 6));
    assert_eq!(issue_stamp(101, WRITER, Some(&last), None).unwrap(), stamp(101, 0));
    assert_eq!(issue_stamp(100, WRITER, Some(&last), Some(&stamp(100, 9))).unwrap(), stamp(100, 10));
    assert_eq!(issue_stamp(90, WRITER, Some(&stamp(100, u32::MAX)), None), Err(WireError("logical-counter-overflow")));
    assert!(stamp(10, 0) < stamp(100, 0));
    let mut other = stamp(100, 0); other.writer_id = "00000000-0000-4000-8000-000000000002".into();
    assert!(stamp(100, 0) < other);
}
fn sample() -> ClockSample {
    ClockSample { target_id: "target".into(), request_wall_ms: 1_000_000, response_wall_ms: 1_000_100,
        request_monotonic_ms: 0, response_monotonic_ms: 100, target_ms: 1_000_050,
        precision_ms: 0, successful: true, cached_or_aged: false }
}
#[test]
fn clock_admission_is_target_specific_fresh_and_includes_uncertainty() {
    let clock = sample().admit().unwrap();
    clock.admit_incoming("target", 100, &stamp(1_300_100, 0)).unwrap();
    assert_eq!(clock.admit_incoming("target", 100, &stamp(1_300_101, 0)), Err(WireError("incoming-clock-skew")));
    clock.admit_incoming("target", 100, &stamp(0, 0)).unwrap();
    assert!(clock.incoming_upper_ms("other", 100).is_err());
    assert!(clock.incoming_upper_ms("target", 900_100).is_err());
    let mut bad = sample(); bad.target_ms += MAX_CLOCK_SKEW_MS;
    assert_eq!(bad.admit().unwrap_err(), WireError("clock-skew"));
    bad = sample(); bad.response_wall_ms += 1001; assert!(bad.admit().is_err());
    bad = sample(); bad.response_monotonic_ms = 0; bad.request_monotonic_ms = 1; assert!(bad.admit().is_err());
    bad = sample(); bad.cached_or_aged = true; assert!(bad.admit().is_err());
    bad = sample(); bad.successful = false; assert!(bad.admit().is_err());
    assert_eq!(ClockSample::http_date_target_ms(1000).unwrap(), 1500);
}
#[test]
fn unit_keys_preserve_components_and_unknown_kinds_without_index_paths() {
    let key = UnitKey::new(&["conversation", "a/b", "chat", "x\"y"]).unwrap();
    assert_eq!(key.components(), ["conversation", "a/b", "chat", "x\"y"]);
    assert!(UnitKey::new(&["future-kind", "opaque", "field"]).is_ok());
    for value in ["[]", "[\"root\", \"x\"]", "[\"root\",1]", "[\"root\"]", "[\"exists\",\"plugins\",\"x\"]"] { assert!(UnitKey::try_from(value.to_owned()).is_err()); }
    assert!(UnitKey::new(&["root", &"x".repeat(MAX_UNIT_KEY_BYTES)]).is_err());
}
#[test]
fn lww_repeats_are_idempotent_and_equal_stamp_integrity_includes_dependencies() {
    let a = UnitValue::inline(br#"{"n":1,"a":true}"#).unwrap();
    let same = UnitValue::inline(br#"{"a":true,"n":1.0}"#).unwrap();
    assert_eq!(a.identity().unwrap(), same.identity().unwrap());
    assert_eq!(compare_version(&stamp(10, 0), &a, &stamp(10, 0), &same).unwrap(), LwwDecision::Identical);
    assert_eq!(compare_version(&stamp(10, 0), &a, &stamp(11, 0), &UnitValue::Deleted).unwrap(), LwwDecision::ApplyRemote);
    assert_eq!(compare_version(&stamp(10, 0), &a, &stamp(9, 0), &UnitValue::Deleted).unwrap(), LwwDecision::KeepLocal);
    assert_eq!(compare_version(&stamp(10, 0), &a, &stamp(10, 0), &UnitValue::Deleted), Err(WireError("equal-stamp-integrity")));
    let descriptor = risunest_sync_wire::descriptor::RecordDescriptor::content("a".repeat(64));
    let a = UnitValue::object(descriptor.clone()).unwrap();
    let mut changed = descriptor; changed.dependencies.push("b".repeat(64));
    let b = UnitValue::object(changed).unwrap();
    assert_eq!(compare_version(&stamp(10, 0), &a, &stamp(10, 0), &b), Err(WireError("equal-stamp-integrity")));
}
#[test]
fn order_keeps_live_ids_once_then_appends_unlisted_creation_order() {
    let live = vec![LiveRecord{id:"b".into(),creation_stamp:stamp(2,0)},LiveRecord{id:"a".into(),creation_stamp:stamp(1,0)},LiveRecord{id:"c".into(),creation_stamp:stamp(1,0)},LiveRecord{id:"retired".into(),creation_stamp:stamp(0,0)}];
    assert_eq!(read_order(&["b".into(),"missing".into(),"b".into(),"retired".into()], &live, &BTreeSet::from(["retired".into()])), ["b","a","c"]);
}
#[test]
fn payload_matches_javascript_keys_numbers_escaping_and_rejects_invalid_json() {
    let bytes = br#"{"z":-0,"10":1e21,"2":1e-7,"01":1e20,"a":[null,true,"\n"]}"#;
    assert_eq!(String::from_utf8(payload_value::canonicalize(bytes).unwrap()).unwrap(), "{\"2\":1e-7,\"10\":1e+21,\"01\":100000000000000000000,\"a\":[null,true,\"\\n\"],\"z\":0}");
    let unicode = "{\"\":1,\"😀\":2}";
    assert_eq!(String::from_utf8(payload_value::canonicalize(unicode.as_bytes()).unwrap()).unwrap(), "{\"😀\":2,\"\":1}");
    for bytes in [br#"{"a":1,"a":2}"#.as_slice(), b"NaN", b"1e400", b"undefined", b"[1,]"] { assert!(payload_value::canonicalize(bytes).is_err()); }
    assert!(canonical::encode(&json!({"n":1})).is_err());
}

#[test]
fn user_key_components_can_be_empty_without_weakening_structural_ids() {
    for components in [vec!["variable", ""], vec!["toggle", "toggle_"], vec!["plugin", "", ""], vec!["plugin-local", "", "v3", ""], vec!["order", "plugin-storage", ""], vec!["record", "plugins", ""]] {
        let key = UnitKey::new(&components).unwrap();
        assert_eq!(UnitKey::try_from(key.as_str().to_owned()).unwrap(), key);
    }
    assert!(UnitKey::new(&["character", "", "name"]).is_err());
    assert!(UnitKey::new(&["", "field"]).is_err());
}

#[test]
fn unknown_existence_kinds_and_order_scopes_are_well_formed_opaque_keys() {
    for components in [vec!["exists", "lorebook", "x"], vec!["exists", "future", "owner", "x"], vec!["order", "lorebooks"], vec!["order", "future", "owner", ""]] {
        let key = UnitKey::new(&components).unwrap();
        assert_eq!(UnitKey::try_from(key.as_str().to_owned()).unwrap(), key);
    }
    for components in [vec!["exists", "lorebook"], vec!["exists", "lorebook", ""], vec!["exists", "", "x"], vec!["exists", "plugins", "x"], vec!["order"], vec!["order", ""]] {
        assert!(UnitKey::new(&components).is_err(), "{components:?}");
    }
}

#[test]
fn toggle_and_variable_keys_are_split_by_the_toggle_prefix() {
    for components in [["toggle", "toggle_mode"], ["toggle", "toggle_"], ["variable", "mode"], ["variable", "toggle"]] {
        assert!(UnitKey::new(&components).is_ok(), "{components:?}");
    }
    for components in [["toggle", "mode"], ["toggle", ""], ["variable", "toggle_mode"]] {
        assert_eq!(UnitKey::new(&components), Err(WireError("invalid-unit-key-shape")), "{components:?}");
    }
}

#[test]
fn decimal_parsing_rounds_to_the_same_ieee_value_as_javascript() {
    assert_eq!(payload_value::canonicalize(b"3.08984926168550152811e-32").unwrap(), b"3.089849261685502e-32");
}

#[test]
fn unit_value_decode_rejects_unknown_fields_for_every_shape() {
    let deleted_with_bytes = br#"{"kind":"deleted","bytes":"ignored"}"#;
    assert!(serde_json::from_slice::<UnitValue>(deleted_with_bytes).is_err());
    assert!(canonical::decode::<UnitValue>(deleted_with_bytes, 4096).is_err());
    for value in [
        UnitValue::Deleted,
        UnitValue::inline(b"null").unwrap(),
        UnitValue::object(risunest_sync_wire::descriptor::RecordDescriptor::content("a".repeat(64))).unwrap(),
    ] {
        let mut input = serde_json::to_value(value).unwrap();
        input.as_object_mut().unwrap().insert("unexpected".into(), json!(true));
        assert!(serde_json::from_value::<UnitValue>(input.clone()).is_err(), "{input}");
        let bytes = serde_json::to_vec(&input).unwrap();
        assert!(canonical::decode::<UnitValue>(&bytes, 4096).is_err(), "{input}");
    }
}

#[test]
fn unit_value_strict_decode_preserves_canonical_wire_roundtrips() {
    assert_eq!(canonical::encode(&UnitValue::Deleted).unwrap(), br#"{"kind":"deleted"}"#);
    assert_eq!(canonical::encode(&UnitValue::inline(b"null").unwrap()).unwrap(), br#"{"bytes":"bnVsbA","kind":"inline"}"#);
    for value in [
        UnitValue::Deleted,
        UnitValue::inline(br#"{"a":1}"#).unwrap(),
        UnitValue::object(risunest_sync_wire::descriptor::RecordDescriptor::content("a".repeat(64))).unwrap(),
    ] {
        let bytes = canonical::encode(&value).unwrap();
        let decoded: UnitValue = canonical::decode(&bytes, 4096).unwrap();
        assert_eq!(decoded, value);
        assert_eq!(decoded.identity().unwrap(), value.identity().unwrap());
        assert_eq!(canonical::encode(&decoded).unwrap(), bytes);
    }
}

#[test]
fn inline_validation_reports_the_first_failing_check() {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    for (bytes, code) in [
        ("!".to_owned(), "invalid-inline-value"),
        (URL_SAFE_NO_PAD.encode(b"not json"), "invalid-payload-json"),
        (URL_SAFE_NO_PAD.encode(br#"{"a":1,"a":2}"#), "invalid-payload-json"),
        (URL_SAFE_NO_PAD.encode(br#"{"b":1,"a":2}"#), "noncanonical-inline-value"),
        (URL_SAFE_NO_PAD.encode(b"[1, 2]"), "noncanonical-inline-value"),
        (format!("{}=", URL_SAFE_NO_PAD.encode(b"null")), "invalid-inline-value"),
    ] {
        let value = UnitValue::Inline { bytes };
        assert_eq!(value.validate(), Err(WireError(code)), "{value:?}");
        assert_eq!(value.validated_inline(), Err(WireError(code)), "{value:?}");
    }
}

#[test]
fn validated_inline_returns_the_decoded_bytes_and_the_json_they_canonicalize_from() {
    let value = UnitValue::inline(br#"{"b":[1,"x"],"a":null}"#).unwrap();
    let (decoded, json) = value.validated_inline().unwrap().unwrap();
    assert_eq!(decoded, br#"{"a":null,"b":[1,"x"]}"#);
    assert_eq!(json, json!({ "a": null, "b": [1.0, "x"] }));
    assert_eq!(payload_value::encode(&json).unwrap(), decoded);
    assert_eq!(payload_value::parse(&decoded).unwrap(), json);
    assert_eq!(UnitValue::Deleted.validated_inline(), Ok(None));
    let object = UnitValue::object(risunest_sync_wire::descriptor::RecordDescriptor::content("a".repeat(64))).unwrap();
    assert_eq!(object.validated_inline(), Ok(None));
}

#[test]
fn inline_values_are_bounded_by_decoded_canonical_bytes() {
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
    let string = |len: usize| format!("\"{}\"", "a".repeat(len - 2)).into_bytes();
    let at_limit = string(MAX_INLINE_UNIT_BYTES);
    let value = UnitValue::inline(&at_limit).unwrap();
    value.validate().unwrap();
    value.identity().unwrap();
    let over = string(MAX_INLINE_UNIT_BYTES + 1);
    assert_eq!(UnitValue::inline(&over).err(), Some(WireError("inline-unit-too-large")));
    let forged = UnitValue::Inline { bytes: URL_SAFE_NO_PAD.encode(&over) };
    assert_eq!(forged.validate(), Err(WireError("inline-unit-too-large")));
    assert_eq!(forged.identity(), Err(WireError("inline-unit-too-large")));
    // The bound applies to the canonical form, so whitespace cannot hide a large value.
    let padded = format!("[{}0]", " ".repeat(MAX_INLINE_UNIT_BYTES)).into_bytes();
    assert_eq!(UnitValue::inline(&padded).unwrap(), UnitValue::inline(b"[0]").unwrap());
}
