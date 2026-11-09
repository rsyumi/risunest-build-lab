//! The `@@` decorator grammar.
//!
//! A port of `CCardLib.decorator.parse` (`@risuai/ccardlib` 0.4.2, `dist/index.js`): the content is trimmed and
//! split on `\n`; each leading line that starts with `@@` after trimming is a decorator `@@name arg, arg`, and the
//! first other line starts the content that is returned. `@@@name` is a fallback that runs only when the previous
//! decorator was refused; `@@@end` always runs as `@@end`. The decorator meanings live in the selector.

use risunest_ir::budget::Budget;
use risunest_ir::stage::StageError;
use risunest_ir::text::{is_whitespace, Text};

const NEWLINE: u16 = b'\n' as u16;

/// Calls `decorator(name, args)` for every decorator line in order and returns the content after them. The callback
/// returns whether it accepted the decorator; a refusal (`return false` in TS) lets the next `@@@` line run.
pub fn parse<F>(content: &Text, budget: &Budget, mut decorator: F) -> Result<Text, StageError>
where F: FnMut(&[u16], &[Text]) -> Result<bool, StageError> {
    let units: Vec<u16> = content.units().collect();
    budget.work(units.len())?;
    let trimmed = trim_units(&units);
    let lines: Vec<&[u16]> = trimmed.split(|unit| *unit == NEWLINE).collect();
    let mut fallback = false;
    for (index, line) in lines.iter().enumerate() {
        budget.work(1)?;
        let mut line = trim_units(line);
        if line == units_of("@@@end").as_slice() { line = &line[1..]; }
        if !line.starts_with(&[b'@' as u16, b'@' as u16]) {
            let mut rest: Vec<u16> = Vec::new();
            for (at, line) in lines[index..].iter().enumerate() {
                if at != 0 { rest.push(NEWLINE); }
                rest.extend_from_slice(line);
            }
            return Ok(Text::from_units(trim_units(&rest), budget)?);
        }
        let triple = line.starts_with(&[b'@' as u16; 3]);
        if triple && !fallback { continue; }
        let space = line.iter().position(|unit| *unit == b' ' as u16).unwrap_or(line.len());
        let start = if triple { 3 } else { 2 };
        let name = if space > start { &line[start..space] } else { &[][..] };
        let mut args = Vec::new();
        for arg in line[space..].split(|unit| *unit == b',' as u16) {
            let arg = trim_units(arg);
            if !arg.is_empty() { args.push(Text::from_units(arg, budget)?); }
        }
        fallback = if name.is_empty() { false } else { !decorator(name, &args)? };
    }
    Ok(Text::from_str("", budget)?)
}

pub(crate) fn units_of(text: &str) -> Vec<u16> { text.encode_utf16().collect() }

/// `String.prototype.trim` over UTF-16 units.
pub(crate) fn trim_units(units: &[u16]) -> &[u16] {
    let start = units.iter().position(|unit| !is_whitespace(*unit)).unwrap_or(units.len());
    let end = units.iter().rposition(|unit| !is_whitespace(*unit)).map_or(start, |index| index + 1);
    &units[start..end]
}

/// `parseInt(arg)` without a radix; `None` is `parseInt(undefined)`.
pub fn parse_int(text: Option<&Text>) -> f64 {
    let Some(text) = text else { return f64::NAN };
    let units: Vec<u16> = text.units().collect();
    let mut rest = &units[units.iter().position(|unit| !is_whitespace(*unit)).unwrap_or(units.len())..];
    let mut negative = false;
    if let Some(&sign) = rest.first() {
        if sign == b'-' as u16 || sign == b'+' as u16 {
            negative = sign == b'-' as u16;
            rest = &rest[1..];
        }
    }
    let mut radix = 10;
    if rest.len() >= 2 && rest[0] == b'0' as u16 && (rest[1] == b'x' as u16 || rest[1] == b'X' as u16) {
        radix = 16;
        rest = &rest[2..];
    }
    let digits: Vec<u8> = rest.iter().map_while(|unit| char::from_u32(*unit as u32).and_then(|c| c.to_digit(radix)))
        .map(|digit| digit as u8).collect();
    if digits.is_empty() { return f64::NAN; }
    let value = if radix == 10 {
        let text: String = digits.iter().map(|digit| (b'0' + digit) as char).collect();
        text.parse::<f64>().unwrap_or(f64::NAN)
    } else {
        hex_value(&digits)
    };
    if negative { -value } else { value }
}

/// The correctly rounded value of hexadecimal digits, as V8 computes it for a power-of-two radix.
fn hex_value(digits: &[u8]) -> f64 {
    let digits = &digits[digits.iter().position(|digit| *digit != 0).unwrap_or(digits.len())..];
    let kept = digits.len().min(32);
    let mut value: u128 = 0;
    for digit in &digits[..kept] { value = (value << 4) | *digit as u128; }
    // The dropped digits only decide rounding, so a nonzero tail sets the lowest kept bit as a sticky bit.
    if digits[kept..].iter().any(|digit| *digit != 0) { value |= 1; }
    let mut result = value as f64;
    for _ in kept..digits.len() { result *= 16.0; }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use risunest_ir::budget::Limits;
    use std::sync::{atomic::AtomicBool, Arc};

    fn budget() -> Budget {
        Budget::new(Limits { max_owned_bytes: 1 << 24, max_json_bytes: 1 << 20, max_text_units: 1 << 20, max_values: 1 << 16,
            max_depth: 32, max_work: 1 << 28 }, Arc::new(AtomicBool::new(false))).unwrap()
    }
    fn text(value: &str, budget: &Budget) -> Text { Text::from_str(value, budget).unwrap() }

    #[test]
    fn parse_int_follows_the_js_prefix_rules() {
        let budget = budget();
        let cases: [(&str, f64); 12] = [("12abc", 12.0), ("  -7", -7.0), ("+3.9", 3.0), ("0x1f", 31.0), ("-0X10", -16.0), ("abc", f64::NAN),
            ("", f64::NAN), ("0x", f64::NAN), ("\u{a0}\u{2028}42", 42.0), ("1e3", 1.0), ("0x1fffffffffffff1", 144115188075855856.0),
            ("123456789012345678901234567890", 1.2345678901234568e29)];
        for (input, expected) in cases {
            let actual = parse_int(Some(&text(input, &budget)));
            assert!(actual == expected || (actual.is_nan() && expected.is_nan()), "{input}: {actual}");
        }
        assert!(parse_int(None).is_nan());
        assert!(parse_int(Some(&text("-0", &budget))).is_sign_negative());
    }
}
