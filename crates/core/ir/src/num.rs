//! ECMAScript number conversions.

use crate::budget::{Budget, Error, OwnedVec};
use crate::text::{is_whitespace, Text};
use crate::value::Value;

/// `Number(text)` for a string operand.
pub fn to_number(text: &Text, budget: &Budget) -> Result<f64, Error> {
    budget.work(text.len_units())?;
    if let Some(value) = text.as_unicode() {
        let literal = value.trim_matches(|character: char| (character as u32) <= 0xffff && is_whitespace(character as u16));
        return Ok(literal_value(literal.as_bytes()));
    }
    // StringNumericLiteral is ASCII between whitespace, so any other unit, an isolated surrogate included, is NaN.
    let mut literal = OwnedVec::new(budget)?;
    let mut trailing = false;
    for (index, unit) in text.units().enumerate() {
        if index & 4095 == 0 { budget.check()?; }
        if is_whitespace(unit) {
            trailing = !literal.as_slice().is_empty();
            continue;
        }
        if trailing || unit > 0x7f { return Ok(f64::NAN); }
        literal.push(unit as u8)?;
    }
    Ok(literal_value(literal.as_slice()))
}

// StrNumericLiteral (ECMA-262 StringToNumber) without the surrounding whitespace.
fn literal_value(literal: &[u8]) -> f64 {
    if literal.is_empty() { return 0.0; }
    if literal.len() >= 2 && literal[0] == b'0' {
        let radix = match literal[1] { b'x' | b'X' => 16, b'o' | b'O' => 8, b'b' | b'B' => 2, _ => 0 };
        if radix != 0 { return radix_value(&literal[2..], radix); }
    }
    let (negative, body) = match literal[0] {
        b'+' => (false, &literal[1..]),
        b'-' => (true, &literal[1..]),
        _ => (false, literal),
    };
    let value = if body == b"Infinity" {
        f64::INFINITY
    } else if decimal_literal(body) {
        // Rust's parser is correctly rounded for any digit count, as StringToNumber requires.
        std::str::from_utf8(body).ok().and_then(|body| body.parse::<f64>().ok()).unwrap_or(f64::NAN)
    } else {
        return f64::NAN;
    };
    if negative { -value } else { value }
}

// StrUnsignedDecimalLiteral other than `Infinity`: `1`, `1.`, `.5`, `1.5e-3`; no numeric separators.
fn decimal_literal(body: &[u8]) -> bool {
    let digits = |from: usize| body[from..].iter().take_while(|byte| byte.is_ascii_digit()).count();
    let integer = digits(0);
    let mut position = integer;
    let mut fraction = 0;
    if body.get(position) == Some(&b'.') {
        position += 1;
        fraction = digits(position);
        position += fraction;
    }
    if integer == 0 && fraction == 0 { return false; }
    if matches!(body.get(position), Some(b'e' | b'E')) {
        position += 1;
        if matches!(body.get(position), Some(b'+' | b'-')) { position += 1; }
        let exponent = digits(position);
        if exponent == 0 { return false; }
        position += exponent;
    }
    position == body.len()
}

// NonDecimalIntegerLiteral digits, rounded to nearest with ties to even like any other number value.
fn radix_value(digits: &[u8], radix: u32) -> f64 {
    if digits.is_empty() { return f64::NAN; }
    let width = radix.trailing_zeros();
    let mut top = 0u64;
    let mut taken = 0u32;
    let mut total = 0usize;
    let mut sticky = false;
    for &byte in digits {
        let Some(digit) = (byte as char).to_digit(radix) else { return f64::NAN };
        for shift in (0..width).rev() {
            let bit = (digit >> shift) & 1;
            if total == 0 && bit == 0 { continue; }
            total += 1;
            if taken < 64 { top = (top << 1) | bit as u64; taken += 1; } else if bit == 1 { sticky = true; }
        }
    }
    if total <= 64 { return top as f64; }
    // The sticky bit sits below the 53-bit rounding position, so it only breaks exact ties.
    if sticky { top |= 1; }
    let scale = total - 64;
    if scale > 1023 { return f64::INFINITY; }
    top as f64 * f64::from_bits(((1023 + scale) as u64) << 52)
}

/// `String(value)` for a number.
pub fn number_to_text(value: f64, budget: &Budget) -> Result<Text, Error> { Value::Number(value).to_text(budget) }

/// ECMAScript `ToUint32`.
pub fn to_uint32(value: f64) -> u32 {
    if !value.is_finite() { return 0; }
    value.trunc().rem_euclid(4_294_967_296.0) as u32
}

/// ECMAScript `ToInt32`.
pub fn to_int32(value: f64) -> i32 { to_uint32(value) as i32 }
