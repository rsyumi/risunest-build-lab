//! UTF-16 helpers with the JS semantics the engine relies on. Every helper charges the budget for the units it reads
//! or writes.

use crate::error::JinjaError;
use crate::CaseTables;
use risunest_ir::num::{number_to_text, to_number};
use risunest_ir::text::is_whitespace;
use risunest_ir::{Budget, Text};
use std::rc::Rc;

pub(crate) type Str = Rc<[u16]>;

pub(crate) fn collect(text: &Text, budget: &Budget) -> Result<Vec<u16>, JinjaError> {
    budget.work(text.len_units())?;
    Ok(text.units().collect())
}

pub(crate) fn s(value: &str) -> Vec<u16> { value.encode_utf16().collect() }
pub(crate) fn rc(value: &str) -> Str { Rc::from(s(value)) }

/// Refuses a string longer than the budget allows, as JS would refuse one past its maximum string length.
pub(crate) fn finish(units: Vec<u16>, budget: &Budget) -> Result<Str, JinjaError> {
    if units.len() > budget.limits().max_text_units { return Err(risunest_ir::Error::Limit("text-units").into()); }
    Ok(Rc::from(units))
}

pub(crate) fn concat(parts: &[&[u16]], budget: &Budget) -> Result<Vec<u16>, JinjaError> {
    let total = parts.iter().map(|part| part.len()).fold(0usize, usize::saturating_add);
    budget.work(total)?;
    if total > budget.limits().max_text_units { return Err(risunest_ir::Error::Limit("text-units").into()); }
    let mut out = Vec::with_capacity(total);
    for part in parts { out.extend_from_slice(part); }
    Ok(out)
}

pub(crate) fn lossy(units: &[u16]) -> String { String::from_utf16_lossy(&units[..units.len().min(200)]) }

pub(crate) fn is_ws(unit: u16) -> bool { is_whitespace(unit) }
/// `/\w/` without the `u` flag.
pub(crate) fn is_word(unit: u16) -> bool { matches!(unit, 0x30..=0x39 | 0x41..=0x5a | 0x61..=0x7a | 0x5f) }
pub(crate) fn is_digit(unit: u16) -> bool { (0x30..=0x39).contains(&unit) }

pub(crate) fn trim_start(units: &[u16]) -> &[u16] {
    let start = units.iter().position(|unit| !is_ws(*unit)).unwrap_or(units.len());
    &units[start..]
}
pub(crate) fn trim_end(units: &[u16]) -> &[u16] {
    let end = units.iter().rposition(|unit| !is_ws(*unit)).map_or(0, |index| index + 1);
    &units[..end]
}
pub(crate) fn trim(units: &[u16]) -> &[u16] { trim_end(trim_start(units)) }

/// `String.prototype.indexOf(needle, from)` over UTF-16 units.
pub(crate) fn find(haystack: &[u16], needle: &[u16], from: usize, budget: &Budget) -> Result<Option<usize>, JinjaError> {
    if needle.len() > haystack.len() { return Ok(None); }
    let last = haystack.len() - needle.len();
    let mut at = from;
    let mut pending = 0usize;
    while at <= last {
        pending += needle.len().max(1);
        if pending >= 4096 { budget.work(pending)?; pending = 0; }
        if &haystack[at..at + needle.len()] == needle { budget.work(pending)?; return Ok(Some(at)); }
        at += 1;
    }
    budget.work(pending)?;
    Ok(None)
}

/// `String.prototype.split(separator)` with a nonempty separator.
pub(crate) fn split<'u>(units: &'u [u16], separator: &[u16], budget: &Budget) -> Result<Vec<&'u [u16]>, JinjaError> {
    let mut parts = Vec::new();
    let mut last = 0;
    while let Some(at) = find(units, separator, last, budget)? {
        budget.work(1)?;
        parts.push(&units[last..at]);
        last = at + separator.len();
    }
    parts.push(&units[last..]);
    Ok(parts)
}

pub(crate) fn join(parts: &[Vec<u16>], separator: &[u16], budget: &Budget) -> Result<Vec<u16>, JinjaError> {
    let mut pieces: Vec<&[u16]> = Vec::with_capacity(parts.len().saturating_mul(2));
    for (index, part) in parts.iter().enumerate() {
        if index != 0 { pieces.push(separator); }
        pieces.push(part);
    }
    concat(&pieces, budget)
}

/// `Array.from(string)`: code points, an isolated surrogate counting as one.
pub(crate) fn code_points(units: &[u16]) -> Vec<&[u16]> {
    let mut points = Vec::new();
    let mut at = 0;
    while at < units.len() {
        let pair = (0xd800..=0xdbff).contains(&units[at]) && units.get(at + 1).is_some_and(|next| (0xdc00..=0xdfff).contains(next));
        let width = if pair { 2 } else { 1 };
        points.push(&units[at..at + width]);
        at += width;
    }
    points
}

/// `toUpperCase` or `toLowerCase`: full Unicode case mapping of each well-formed run; isolated surrogates stay. A text the
/// engine's `tables` could map differently declines.
pub(crate) fn case_map(units: &[u16], upper: bool, tables: CaseTables, budget: &Budget) -> Result<Vec<u16>, JinjaError> {
    budget.work(units.len())?;
    if tables.differs(units) { return Err(JinjaError::Unsupported("jinja-case-table")); }
    let mut out = Vec::with_capacity(units.len());
    let mut run_start = 0;
    let mut at = 0;
    let flush = |run: &[u16], out: &mut Vec<u16>| {
        if run.is_empty() { return; }
        let text = String::from_utf16(run).expect("well-formed run");
        let mapped = if upper { text.to_uppercase() } else { text.to_lowercase() };
        out.extend(mapped.encode_utf16());
    };
    while at < units.len() {
        let unit = units[at];
        if (0xd800..=0xdbff).contains(&unit) && units.get(at + 1).is_some_and(|next| (0xdc00..=0xdfff).contains(next)) {
            at += 2;
            continue;
        }
        if (0xd800..=0xdfff).contains(&unit) {
            flush(&units[run_start..at], &mut out);
            out.push(unit);
            at += 1;
            run_start = at;
            continue;
        }
        at += 1;
    }
    flush(&units[run_start..], &mut out);
    budget.work(out.len())?;
    if out.len() > budget.limits().max_text_units { return Err(risunest_ir::Error::Limit("text-units").into()); }
    Ok(out)
}

/// `JSON.stringify(string)`.
pub(crate) fn json_quote(units: &[u16], out: &mut Vec<u16>, budget: &Budget) -> Result<(), JinjaError> {
    budget.work(units.len().saturating_add(2))?;
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let escape = |unit: u16, out: &mut Vec<u16>| {
        out.extend([0x5c, 0x75]);
        for shift in [12, 8, 4, 0] { out.push(HEX[((unit >> shift) & 0xf) as usize] as u16); }
    };
    out.push(0x22);
    let mut at = 0;
    while at < units.len() {
        let unit = units[at];
        match unit {
            0x22 => out.extend([0x5c, 0x22]),
            0x5c => out.extend([0x5c, 0x5c]),
            0x08 => out.extend([0x5c, 0x62]),
            0x0c => out.extend([0x5c, 0x66]),
            0x0a => out.extend([0x5c, 0x6e]),
            0x0d => out.extend([0x5c, 0x72]),
            0x09 => out.extend([0x5c, 0x74]),
            0..=0x1f => escape(unit, out),
            0xd800..=0xdbff if units.get(at + 1).is_some_and(|next| (0xdc00..=0xdfff).contains(next)) => {
                out.extend([unit, units[at + 1]]);
                at += 1;
            }
            0xd800..=0xdfff => escape(unit, out),
            _ => out.push(unit),
        }
        at += 1;
    }
    out.push(0x22);
    Ok(())
}

/// `String(number)`.
pub(crate) fn number(value: f64, budget: &Budget) -> Result<Vec<u16>, JinjaError> {
    Ok(collect(&number_to_text(value, budget)?, budget)?)
}

/// `FloatValue.toString`: `-0.0`, `n.toFixed(1)` for integral values, `String(n)` otherwise.
pub(crate) fn float(value: f64, budget: &Budget) -> Result<Vec<u16>, JinjaError> {
    if value == 0.0 && value.is_sign_negative() { return Ok(s("-0.0")); }
    if value.is_finite() && value % 1.0 == 0.0 {
        // toFixed(1) prints the exact integer below 1e21 and String(n) from there on.
        if value.abs() >= 1e21 { return number(value, budget); }
        let digits = (value.abs() as u128).to_string();
        let sign = if value < 0.0 { "-" } else { "" };
        return Ok(s(&format!("{sign}{digits}.0")));
    }
    number(value, budget)
}

/// `Number(string)`.
pub(crate) fn string_to_number(units: &[u16], budget: &Budget) -> Result<f64, JinjaError> {
    Ok(to_number(&Text::from_units(units, budget)?, budget)?)
}

/// `parseInt(string, 10)`.
pub(crate) fn parse_int(units: &[u16]) -> f64 {
    let rest = trim_start(units);
    let (negative, rest) = match rest.first() {
        Some(0x2d) => (true, &rest[1..]),
        Some(0x2b) => (false, &rest[1..]),
        _ => (false, rest),
    };
    let digits = rest.iter().take_while(|unit| is_digit(**unit)).count();
    if digits == 0 { return f64::NAN; }
    let text: String = rest[..digits].iter().map(|unit| *unit as u8 as char).collect();
    let value: f64 = text.parse().unwrap_or(f64::NAN);
    if negative { -value } else { value }
}

/// `parseFloat(string)`: the longest StrDecimalLiteral prefix after leading whitespace.
pub(crate) fn parse_float(units: &[u16]) -> f64 {
    let rest = trim_start(units);
    let mut at = 0;
    let negative = match rest.first() {
        Some(0x2d) => { at = 1; true }
        Some(0x2b) => { at = 1; false }
        _ => false,
    };
    let infinity = s("Infinity");
    if rest[at..].starts_with(&infinity) { return if negative { f64::NEG_INFINITY } else { f64::INFINITY }; }
    let digits = |from: usize| rest[from..].iter().take_while(|unit| is_digit(**unit)).count();
    let start = at;
    let whole = digits(at);
    at += whole;
    let mut fraction = 0;
    if rest.get(at) == Some(&0x2e) {
        fraction = digits(at + 1);
        if whole + fraction > 0 { at += 1 + fraction; }
    }
    if whole + fraction == 0 { return f64::NAN; }
    if matches!(rest.get(at), Some(0x65 | 0x45)) {
        let mut exponent = at + 1;
        if matches!(rest.get(exponent), Some(0x2b | 0x2d)) { exponent += 1; }
        let count = digits(exponent);
        if count > 0 { at = exponent + count; }
    }
    let text: String = rest[start..at].iter().map(|unit| *unit as u8 as char).collect();
    let value: f64 = text.parse().unwrap_or(f64::NAN);
    if negative { -value } else { value }
}
