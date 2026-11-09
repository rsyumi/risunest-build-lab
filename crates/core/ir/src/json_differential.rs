//! Parses a seeded corpus with the current parser and with `reference`, the parser before its fast paths, and requires
//! the same result: the same value with the same storage and capacities, or the same error, and the same budget state
//! (owned bytes while the value lives and after it drops, peak bytes, and the `work` and `json-values` counters), under
//! generous limits and under limits cut just below what each document needs.

use super::reference;
use crate::budget::{Budget, Error, Limits};
use crate::text::{Text, TextData};
use crate::value::Value;
use std::fmt::Write;
use std::sync::{atomic::AtomicBool, Arc};

const GENEROUS: Limits = Limits { max_owned_bytes: 1 << 30, max_json_bytes: 1 << 24, max_text_units: 1 << 24, max_values: 1 << 22,
    max_depth: 128, max_work: 1 << 40 };

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, bound: u64) -> u64 { self.next() % bound }
    fn range(&mut self, low: usize, high: usize) -> usize { low + self.below((high - low + 1) as u64) as usize }
    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str { items[self.below(items.len() as u64) as usize] }
}

fn push(output: &mut Vec<u16>, text: &str) { output.extend(text.encode_utf16()); }

const PLAIN: &[&str] = &["a", "b", "z", "0", "9", " ", "-", ".", "A", "Z", "~", "\u{7f}", "\u{80}", "\u{9f}", "é", "Ж", "\u{7ff}", "\u{800}",
    "가", "한", "\u{d000}", "\u{d7ff}", "\u{e000}", "中", "\u{fffd}", "\u{ffff}", "🐿", "\u{10000}", "\u{10ffff}"];
const SHORT: &[&str] = &["\\n", "\\t", "\\\"", "\\\\", "\\/", "\\b", "\\f", "\\r"];
const HEX: &[&str] = &["\\u0000", "\\u001f", "\\u0020", "\\u0041", "\\u007f", "\\u0080", "\\u00e9", "\\u07FF", "\\u0800", "\\uAbCd",
    "\\ud7ff", "\\ue000", "\\uffff", "\\ud83d\\ude00", "\\uD800\\uDC00", "\\udbff\\udfff"];
// Escapes that leave an isolated surrogate, or pair only across raw and escaped units.
const LONE: &[&str] = &["\\ud800", "\\udbff", "\\udc00", "\\udfff", "\\ud83d\\u0041", "\\ud83dx", "\\ud800\\ud800", "\\udc00\\ud800"];
const INVALID: &[&str] = &["\u{1}", "\u{1f}", "\n", "\\x", "\\u12", "\\u12g4", "\\U0041", "\\", "\\'"];

fn string(rng: &mut Rng, output: &mut Vec<u16>, raw_surrogates: bool) {
    output.push(34);
    let count = match rng.below(10) { 0 => 0, 1..=5 => rng.range(1, 8), 6..=8 => rng.range(8, 70), _ => rng.range(70, 300) };
    for _ in 0..count {
        match rng.below(if raw_surrogates { 28 } else { 24 }) {
            0..=9 => push(output, rng.pick(PLAIN)),
            10..=12 => push(output, rng.pick(SHORT)),
            13..=15 => push(output, rng.pick(HEX)),
            16 => push(output, rng.pick(LONE)),
            17 if rng.below(6) == 0 => push(output, rng.pick(INVALID)),
            18 | 19 => for _ in 0..rng.range(20, 90) { output.push(b"abcdefgh xyz"[rng.below(12) as usize] as u16) },
            20..=23 => push(output, rng.pick(&["한국어", "다람쥐 헌", "🐿️", "\u{d55c}\u{d7a3}"])),
            24 | 25 => output.push([0xd800, 0xdbff, 0xdc00, 0xdfff][rng.below(4) as usize]),
            26 => { output.push(0xd83d); push(output, "\\ude00"); }
            27 => { push(output, "\\ud83d"); output.push(0xde00); }
            _ => push(output, "q"),
        }
    }
    output.push(34);
}

const KEYS: &[&str] = &["\"a\"", "\"b\"", "\"0\"", "\"1\"", "\"2\"", "\"10\"", "\"01\"", "\"4294967294\"", "\"4294967295\"", "\"__proto__\"",
    "\"\"", "\"키\"", "\"🐿\"", "\"a\\u0000\"", "\"\\u0061\"", "\"9\"", "\"007\""];
const NUMBERS: &[&str] = &["0", "-0", "1", "-1", "1.5", "-0.0", "1e400", "-1e400", "1e-400", "9007199254740993", "0.1", "1E+2", "1e-2",
    "123456789012345678901234567890", "2.2250738585072014e-308", "4.9e-324", "1.7976931348623157e308", "0.30000000000000004",
    "01", "1.", ".5", "+1", "-", "1e", "1e+", "--1", "0x10", "1.e3", "-01"];

fn number(rng: &mut Rng, output: &mut Vec<u16>) {
    if rng.below(3) == 0 { return push(output, rng.pick(NUMBERS)); }
    let bits = rng.below(60);
    let mut text = rng.below(1 << bits).to_string();
    if rng.below(2) == 0 { text.insert(0, '-'); }
    if rng.below(2) == 0 { write!(text, ".{}", rng.below(1_000_000_000)).unwrap(); }
    if rng.below(4) == 0 { write!(text, "e{}{}", ["", "+", "-"][rng.below(3) as usize], rng.below(400)).unwrap(); }
    push(output, &text);
}

fn space(rng: &mut Rng, output: &mut Vec<u16>) {
    if rng.below(3) == 0 { push(output, rng.pick(&["", " ", "\n", "\t", "\r\n", "  \n  ", "\u{a0}"])); }
}

fn value(rng: &mut Rng, output: &mut Vec<u16>, depth: usize, raw_surrogates: bool) {
    space(rng, output);
    match rng.below(if depth > 6 { 6 } else { 9 }) {
        0 => push(output, rng.pick(&["null", "true", "false", "nul", "tru"])),
        1 | 2 => number(rng, output),
        3..=5 => string(rng, output, raw_surrogates),
        6 | 7 => {
            output.push(91);
            let count = if rng.below(40) == 0 { rng.range(100, 1500) } else { rng.range(0, 5) };
            for index in 0..count {
                if index != 0 { output.push(44); }
                if count > 50 { number(rng, output) } else { value(rng, output, depth + 1, raw_surrogates) }
            }
            space(rng, output);
            output.push(93);
        }
        _ => {
            output.push(123);
            let count = if rng.below(30) == 0 { rng.range(20, 300) } else { rng.range(0, 6) };
            for index in 0..count {
                if index != 0 { output.push(44); }
                space(rng, output);
                // Large objects draw from small pools, so duplicates and index keys follow many members.
                match if count > 6 { rng.below(6) } else { rng.below(4) } {
                    0 => string(rng, output, raw_surrogates),
                    4 => push(output, &format!("\"k{}\"", rng.below(48))),
                    5 => push(output, &format!("\"{}\"", [rng.below(120), rng.below(1 << 33)][rng.below(2) as usize])),
                    _ if rng.below(30) == 0 => push(output, &format!("\"{}\"", "x".repeat(rng.range(250, 262)))),
                    _ => push(output, rng.pick(KEYS)),
                }
                space(rng, output);
                output.push(58);
                value(rng, output, depth + 1, raw_surrogates);
            }
            space(rng, output);
            output.push(125);
        }
    }
    space(rng, output);
}

// A canonical rendering of a value with its storage: number bits, each text's storage, units and capacity, and each
// array's and object's capacity.
fn render(value: &Value, output: &mut String) {
    match value {
        Value::Undefined => output.push('u'),
        Value::Null => output.push('n'),
        Value::Bool(value) => output.push(if *value { 't' } else { 'f' }),
        Value::Number(value) => write!(output, "#{:016x}", value.to_bits()).unwrap(),
        Value::Text(text) => render_text(text, output),
        Value::Array(values) => {
            write!(output, "[{};", values.data.capacity()).unwrap();
            for value in values.as_slice() { render(value, output); output.push(','); }
            output.push(']');
        }
        Value::Object(values) => {
            write!(output, "{{{};", values.data.capacity()).unwrap();
            for (key, value) in values.as_slice() { render_text(key, output); output.push(':'); render(value, output); output.push(','); }
            output.push('}');
        }
    }
}
fn render_text(text: &Text, output: &mut String) {
    let (storage, capacity) = match &text.data { TextData::Utf8(data) => ("8", data.data.capacity()), TextData::Utf16(data) => ("16", data.data.capacity()) };
    write!(output, "s{storage}/{}/{capacity}{:?}", text.len_units(), text.units().collect::<Vec<_>>()).unwrap();
}

#[derive(Debug, PartialEq)]
struct Outcome { result: Result<String, Error>, owned: usize, peak: usize, counters: (usize, usize), released: usize }

fn observe(limits: Limits, cancelled: bool, parse: impl FnOnce(&Budget) -> Result<Value, Error>) -> Outcome {
    let budget = Budget::new(limits, Arc::new(AtomicBool::new(cancelled))).unwrap();
    let parsed = parse(&budget);
    let (owned, peak, counters) = (budget.owned_bytes(), budget.peak_bytes(), budget.counters());
    let result = parsed.map(|value| { let mut rendered = String::new(); render(&value, &mut rendered); rendered });
    Outcome { result, owned, peak, counters, released: budget.owned_bytes() }
}

// The document as both parsers' entry points take it: a `str` when its units are well formed, otherwise a `Text` that
// `parse_json_text` copies to WTF-8.
fn compare(units: &[u16], limits: Limits, cancelled: bool) -> Outcome {
    let rich = Budget::new(GENEROUS, Arc::new(AtomicBool::new(false))).unwrap();
    let text = Text::from_units(units, &rich).unwrap();
    let (old, new) = match String::from_utf16(units) {
        Ok(source) => {
            let old = observe(limits, cancelled, |budget| reference::parse_json(&source, budget));
            let new = observe(limits, cancelled, |budget| super::parse_json(&source, budget));
            // A well-formed `Text` goes through `parse_json` as well.
            assert_eq!(observe(limits, cancelled, |budget| super::parse_json_text(&text, budget)), new, "parse_json_text {source:?}");
            (old, new)
        }
        Err(_) => (observe(limits, cancelled, |budget| reference::parse_json_text(&text, budget)),
            observe(limits, cancelled, |budget| super::parse_json_text(&text, budget))),
    };
    assert_eq!(old, new, "{limits:?} {:?}", String::from_utf16_lossy(units));
    assert_eq!(new.released, Budget::context_bytes());
    new
}

// Limits each cut below what the document used under generous ones, so the parse fails at a varying point.
fn tight(rng: &mut Rng, used: &Outcome, units: usize) -> Limits {
    let mut limits = GENEROUS;
    for _ in 0..rng.range(1, 3) {
        match rng.below(6) {
            0 => limits.max_work = rng.range(1, used.counters.0 + 2),
            1 => limits.max_values = rng.range(1, used.counters.1 + 1),
            2 => limits.max_owned_bytes = Budget::context_bytes() + rng.range(0, used.peak - Budget::context_bytes() + 8),
            3 => limits.max_text_units = rng.range(1, units + 2),
            4 => limits.max_json_bytes = rng.range(1, 3 * units + 2),
            _ => limits.max_depth = rng.range(0, 8),
        }
    }
    limits
}

#[test]
fn parse_matches_the_reference_on_a_seeded_corpus() {
    let mut rng = Rng(0x5eed_1234_abcd_ef01);
    let mut failures = 0usize;
    for document in 0..3000 {
        let raw_surrogates = document % 4 == 3;
        let mut units = Vec::new();
        value(&mut rng, &mut units, 0, raw_surrogates);
        if rng.below(20) == 0 { push(&mut units, rng.pick(&[" x", ",", "]", " true", "\u{0}"])); }
        let used = compare(&units, GENEROUS, false);
        failures += usize::from(used.result.is_err());
        if units.len() <= 80 {
            for end in 0..units.len() { compare(&units[..end], GENEROUS, false); }
        } else {
            for _ in 0..8 { compare(&units[..rng.range(0, units.len())], GENEROUS, false); }
        }
        for _ in 0..3 {
            let mut mutated = units.clone();
            let at = rng.range(0, units.len() - 1);
            mutated[at] = [34, 92, 117, 48, 44, 93, 125, 58, 32, 1, 120, 0xd800, 0xdc00][rng.below(13) as usize];
            compare(&mutated, GENEROUS, false);
        }
        for _ in 0..4 { compare(&units, tight(&mut rng, &used, units.len()), false); }
        if document % 50 == 0 { assert_eq!(compare(&units, GENEROUS, true).result, Err(Error::Cancelled)); }
    }
    // The corpus must reach both outcomes.
    println!("{failures} of 3000 documents fail to parse");
    assert!((300..2700).contains(&failures), "{failures} of 3000 documents fail to parse");
}

// Long documents cross the meter's lease blocks (65536 work units, 1024 values); every limit at or near the boundary
// it ends on gives the same outcome.
#[test]
fn parse_matches_the_reference_at_every_limit_near_the_end() {
    let rich = Budget::new(GENEROUS, Arc::new(AtomicBool::new(false))).unwrap();
    let mut rng = Rng(0x0dd_ba11);
    for raw_surrogates in [false, true] {
        // An array of the generated values that parse, each followed by small values so the count crosses value blocks.
        let mut units = vec![91];
        let mut items = 0;
        while units.len() < 60_000 || items < 400 {
            let mut item = Vec::new();
            value(&mut rng, &mut item, 1, raw_surrogates);
            if super::parse_json_text(&Text::from_units(&item, &rich).unwrap(), &rich).is_err() { continue; }
            if units.len() > 1 { units.push(44); }
            units.extend(item);
            push(&mut units, ",0,-1,2.5,[],{},\"x\"");
            items += 1;
        }
        units.push(93);
        let used = compare(&units, GENEROUS, false);
        assert!(used.result.is_ok(), "{:?}", used.result);
        let (work, values) = used.counters;
        assert!(work > 2 << 16 && values > 2 << 10, "{work} {values}");
        let mut limits = GENEROUS;
        for max_work in (work - 20..=work + 1).chain([1 << 16, (1 << 16) + 1, 3 << 16, work / 2]) {
            limits.max_work = max_work;
            compare(&units, limits, false);
        }
        limits = GENEROUS;
        for max_values in (values - 3..=values + 1).chain([1024, 1025, 2048, values / 2]) {
            limits.max_values = max_values;
            compare(&units, limits, false);
        }
        limits = GENEROUS;
        for max_owned in (used.peak - 400..=used.peak).step_by(13).chain([used.peak / 2, used.peak / 3]) {
            limits.max_owned_bytes = max_owned;
            compare(&units, limits, false);
        }
    }
}

// Objects past `INDEXED` members whose keys and values all parse, so each reaches its last member: duplicate and new
// keys of every kind go through the key index, and since key lookups make most of such a document's work, most work
// limits stop inside one, where the exact per-comparison charges take over.
#[test]
fn wide_objects_match_the_reference_at_work_limits_inside_key_lookups() {
    let rich = Budget::new(GENEROUS, Arc::new(AtomicBool::new(false))).unwrap();
    let parses = |units: &[u16]| super::parse_json_text(&Text::from_units(units, &rich).unwrap(), &rich).is_ok();
    let mut rng = Rng(0x71de_0b1e);
    for document in 0..24 {
        let raw_surrogates = document % 4 == 3;
        let count = rng.range(32, 300);
        let (mut units, mut members) = (vec![123], 0);
        while members < count {
            let mut key = Vec::new();
            match rng.below(8) {
                0 | 1 => string(&mut rng, &mut key, raw_surrogates),
                2 => push(&mut key, &format!("\"k{}\"", rng.below(48))),
                3 => push(&mut key, &format!("\"{}\"", [rng.below(120), rng.below(1 << 33)][rng.below(2) as usize])),
                4 => push(&mut key, &format!("\"{}\"", "x".repeat([rng.range(250, 262), rng.range(300, 700)][rng.below(2) as usize]))),
                _ => push(&mut key, rng.pick(KEYS)),
            }
            let mut item = Vec::new();
            value(&mut rng, &mut item, 5, raw_surrogates);
            if !parses(&key) || !parses(&item) { continue; }
            if members != 0 { units.push(44); }
            units.extend(key);
            units.push(58);
            units.extend(item);
            members += 1;
        }
        units.push(125);
        let used = compare(&units, GENEROUS, false);
        assert!(used.result.is_ok(), "{:?}", used.result);
        let mut limits = GENEROUS;
        for _ in 0..40 {
            limits.max_work = rng.range(1, used.counters.0);
            compare(&units, limits, false);
        }
    }
}

// Each string's growth charge is emulated in one step; every owned limit within one string's growth must fail where
// the doubling would.
#[test]
fn string_growth_matches_the_reference_at_every_owned_limit() {
    let mut rng = Rng(0x9a0d);
    for _ in 0..40 {
        let mut units = Vec::new();
        string(&mut rng, &mut units, false);
        let used = compare(&units, GENEROUS, false);
        let mut limits = GENEROUS;
        for max_owned in Budget::context_bytes()..=used.peak {
            limits.max_owned_bytes = max_owned;
            compare(&units, limits, false);
        }
        for max_work in 1..=used.counters.0 {
            limits = GENEROUS;
            limits.max_work = max_work;
            compare(&units, limits, false);
        }
    }
}

// The fast number path against `str::parse` on tokens around its bounds: 2^53, nineteen digits and powers of ten near 22.
#[test]
fn exact_numbers_equal_the_correctly_rounded_parse() {
    let mut rng = Rng(0xf1_0a7);
    let mut fast = 0usize;
    for _ in 0..200_000 {
        let mut token = String::new();
        if rng.below(2) == 0 { token.push('-'); }
        let digits = rng.range(1, 21);
        let point = rng.range(0, digits);
        for index in 0..digits {
            if index == point && index != 0 { token.push('.'); }
            token.push(char::from(b'0' + if index == 0 && digits > 1 && point != 1 { rng.range(1, 9) } else { rng.range(0, 9) } as u8));
        }
        if rng.below(2) == 0 { write!(token, "{}{}", ["e", "E", "e+", "e-"][rng.below(4) as usize], rng.range(0, 30)).unwrap(); }
        let expected = token.parse::<f64>().unwrap();
        if let Some(value) = super::exact_number(token.as_bytes()) {
            fast += 1;
            assert_eq!(value.to_bits(), expected.to_bits(), "{token}");
        }
    }
    for token in ["9007199254740992", "9007199254740993", "-0", "0e30", "1e22", "1e23", "123456789012345e-22", "0.0000000000000000000001"] {
        if let Some(value) = super::exact_number(token.as_bytes()) { assert_eq!(value.to_bits(), token.parse::<f64>().unwrap().to_bits(), "{token}"); }
    }
    assert!(fast > 50_000, "{fast} tokens took the fast path");
}

fn observe_text(limits: Limits, write: impl FnOnce(&Budget) -> Result<Option<Text>, Error>) -> Outcome {
    let budget = Budget::new(limits, Arc::new(AtomicBool::new(false))).unwrap();
    let written = write(&budget);
    let (owned, peak, counters) = (budget.owned_bytes(), budget.peak_bytes(), budget.counters());
    let result = written.map(|text| { let mut rendered = String::new(); if let Some(text) = text { render_text(&text, &mut rendered); } rendered });
    Outcome { result, owned, peak, counters, released: budget.owned_bytes() }
}

fn compare_stringify(value: &Value, limits: Limits) -> Outcome {
    let old = observe_text(limits, |budget| reference::stringify(value, budget));
    let new = observe_text(limits, |budget| super::stringify(value, budget));
    assert_eq!(old, new, "{limits:?}");
    new
}

// Parsed corpus values, some wrapped with `undefined` elements and members, written by both writers under generous and
// cut limits.
#[test]
fn stringify_matches_the_reference_on_a_seeded_corpus() {
    let rich = Budget::new(GENEROUS, Arc::new(AtomicBool::new(false))).unwrap();
    let mut rng = Rng(0x57_2141);
    let mut written = 0usize;
    for document in 0..2500 {
        let mut units = Vec::new();
        value(&mut rng, &mut units, 0, document % 4 == 3);
        let Ok(mut parsed) = super::parse_json_text(&Text::from_units(&units, &rich).unwrap(), &rich) else { continue };
        if document % 5 == 0 {
            let mut items = crate::budget::OwnedVec::new(&rich).unwrap();
            items.push(Value::Undefined).unwrap();
            items.push(parsed).unwrap();
            let mut members = crate::budget::OwnedVec::new(&rich).unwrap();
            members.push((Text::from_str("u", &rich).unwrap(), Value::Undefined)).unwrap();
            members.push((Text::from_str("v", &rich).unwrap(), Value::Array(items))).unwrap();
            members.push((Text::from_str("w", &rich).unwrap(), Value::Undefined)).unwrap();
            parsed = Value::Object(members);
        }
        let used = compare_stringify(&parsed, GENEROUS);
        written += usize::from(used.result.is_ok());
        for _ in 0..4 {
            let mut limits = GENEROUS;
            match rng.below(4) {
                0 => limits.max_work = rng.range(1, used.counters.0 + 2),
                1 => limits.max_owned_bytes = Budget::context_bytes() + rng.range(0, used.peak - Budget::context_bytes() + 8),
                2 => limits.max_text_units = rng.range(1, units.len() * 2 + 4),
                _ => limits.max_depth = rng.range(0, 8),
            }
            compare_stringify(&parsed, limits);
        }
    }
    assert!(compare_stringify(&Value::Undefined, GENEROUS).result.is_ok());
    assert!(written > 1000, "{written} values written");
}
