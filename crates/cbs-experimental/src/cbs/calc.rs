// Port of calculateCbsExpression (src/ts/process/cbsArithmetic.ts) over UTF-16 code units.
// A value of None is JavaScript undefined, which the shunting-yard stack can produce.
use super::{number_string, units, ws, Budget, Units};

const OPERATORS: [u16; 15] = [43, 45, 42, 47, 94, 37, 60, 62, 124, 38, 0x2264, 0x2265, 61, 0x2260, 33];
fn operator(c: u16) -> bool {
    OPERATORS.contains(&c)
}
fn precedence(c: u16) -> u8 {
    match c {
        43 | 45 => 2,
        42 | 47 | 37 => 3,
        94 => 4,
        33 => 5,
        _ => 1,
    }
}

// parseFloat: the longest StrDecimalLiteral prefix after leading white space.
pub(super) fn parse_float(s: &[u16]) -> f64 {
    let s = super::text::trim_start(s);
    let mut i = 0;
    if matches!(s.first(), Some(43 | 45)) {
        i = 1;
    }
    let infinity = units("Infinity");
    if s[i..].starts_with(&infinity) {
        return if s.first() == Some(&45) { f64::NEG_INFINITY } else { f64::INFINITY };
    }
    let digit = |c: Option<&u16>| c.is_some_and(|c| (48..=57).contains(c));
    let start = i;
    while digit(s.get(i)) {
        i += 1
    }
    let mut digits = i - start;
    if s.get(i) == Some(&46) {
        let dot = i + 1;
        let mut j = dot;
        while digit(s.get(j)) {
            j += 1
        }
        digits += j - dot;
        if digits > 0 {
            i = j
        }
    }
    if digits == 0 {
        return f64::NAN;
    }
    if matches!(s.get(i), Some(101 | 69)) {
        let mut j = i + 1;
        if matches!(s.get(j), Some(43 | 45)) {
            j += 1
        }
        let exponent = j;
        while digit(s.get(j)) {
            j += 1
        }
        if j > exponent {
            i = j
        }
    }
    let text: String = s[..i].iter().map(|&c| c as u8 as char).collect();
    text.parse().unwrap_or(f64::NAN)
}
fn number(token: &[u16]) -> Option<f64> {
    let n = parse_float(token);
    // `parseFloat(token) || token === '0'`
    if (n != 0.0 && !n.is_nan()) || token == [48] {
        Some(n)
    } else {
        None
    }
}
fn truthy(v: Option<f64>) -> bool {
    v.is_some_and(|n| n != 0.0 && !n.is_nan())
}
fn bit(v: bool) -> Option<f64> {
    Some(if v { 1.0 } else { 0.0 })
}
fn odd_integer(n: f64) -> bool {
    n.fract() == 0.0 && (n / 2.0).fract() != 0.0
}
// A finite nonzero value as odd * 2^exponent.
fn odd_scaled(n: f64) -> (i64, i32) {
    let bits = n.to_bits();
    let raw = ((bits >> 52) & 0x7ff) as i32;
    let fraction = (bits & ((1 << 52) - 1)) as i64;
    let (mut odd, mut exponent) = if raw == 0 { (fraction, -1074) } else { (fraction | 1 << 52, raw - 1075) };
    let shift = odd.trailing_zeros();
    odd >>= shift;
    exponent += shift as i32;
    (if n < 0.0 { -odd } else { odd }, exponent)
}
// 2^exponent for a normal exponent.
fn two_to(exponent: i32) -> f64 {
    f64::from_bits(((exponent + 1023) as u64) << 52)
}
// Number::exponentiate, admitted only where the result is exact: the specified special cases,
// an integer to a positive integer power with a representable result, and a power of two to an
// integer power within the normal range. Engines only approximate every other power.
pub(super) fn power(a: f64, b: f64) -> Result<f64, String> {
    if b.is_nan() {
        return Ok(f64::NAN);
    }
    if b == 0.0 {
        return Ok(1.0);
    }
    if a.is_nan() {
        return Ok(f64::NAN);
    }
    if a.is_infinite() || a == 0.0 {
        let positive = b > 0.0;
        let negative_base = a.is_sign_negative();
        let huge = a.is_infinite() == positive;
        let sign = negative_base && odd_integer(b);
        let magnitude = if huge { f64::INFINITY } else { 0.0 };
        return Ok(if sign { -magnitude } else { magnitude });
    }
    if b.is_infinite() {
        let m = a.abs();
        return Ok(if m == 1.0 { f64::NAN } else if (m > 1.0) == (b > 0.0) { f64::INFINITY } else { 0.0 });
    }
    if a < 0.0 && b.fract() != 0.0 {
        return Ok(f64::NAN);
    }
    if b.fract() != 0.0 {
        return Err("pow-inexact".into());
    }
    let (odd, exponent) = odd_scaled(a);
    if odd.abs() == 1 {
        // A power of two stays a power of two; every double this large is an even integer.
        let scale = f64::from(exponent) * b;
        if !(-1022.0..=1023.0).contains(&scale) {
            return Err("pow-inexact".into());
        }
        let value = two_to(scale as i32);
        return Ok(if odd < 0 && b % 2.0 != 0.0 { -value } else { value });
    }
    if !(0.0..=64.0).contains(&b) || exponent < 0 {
        return Err("pow-inexact".into());
    }
    let mut magnitude: i128 = 1;
    for _ in 0..b as u32 {
        magnitude = match magnitude.checked_mul(i128::from(odd)) {
            Some(m) if m.unsigned_abs() < 1 << 53 => m,
            _ => return Err("pow-inexact".into()),
        };
    }
    let scale = i64::from(exponent) * b as i64;
    if scale + 127 - i64::from(magnitude.unsigned_abs().leading_zeros()) > 1023 {
        return Err("pow-inexact".into());
    }
    Ok(magnitude as f64 * two_to(scale as i32))
}
fn apply(op: u16, a: Option<f64>, b: Option<f64>) -> Result<Option<f64>, String> {
    let n = |v: Option<f64>| v.unwrap_or(f64::NAN);
    Ok(match op {
        43 => Some(n(a) + n(b)),
        45 => Some(n(a) - n(b)),
        42 => Some(n(a) * n(b)),
        47 => Some(n(a) / n(b)),
        94 => Some(power(n(a), n(b))?),
        37 => Some(n(a) % n(b)),
        60 => bit(n(a) < n(b)),
        62 => bit(n(a) > n(b)),
        124 => if truthy(a) { a } else { b },
        38 => if truthy(a) { b } else { a },
        0x2264 => bit(n(a) <= n(b)),
        0x2265 => bit(n(a) >= n(b)),
        61 => bit(match (a, b) { (Some(x), Some(y)) => x == y, (None, None) => true, _ => false }),
        0x2260 => bit(!match (a, b) { (Some(x), Some(y)) => x == y, (None, None) => true, _ => false }),
        33 => bit(!truthy(b)),
        _ => return Ok(None),
    })
}
fn replace_pairs(s: &[u16]) -> Units {
    let mut s = s.to_vec();
    for (from, to) in [("&&", "&"), ("||", "|"), ("<=", "≤"), (">=", "≥"), ("==", "="), ("!=", "≠")] {
        s = super::replace_literal(&s, &units(from), &units(to));
    }
    // /null/gi without the unicode flag folds ASCII letters only.
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if i + 4 <= s.len() && s[i..i + 4].iter().zip(b"null").all(|(&c, &l)| c < 128 && (c as u8).to_ascii_lowercase() == l) {
            out.push(48);
            i += 4;
        } else {
            out.push(s[i]);
            i += 1;
        }
    }
    out
}
fn execute(text: &[u16], budget: &mut Budget) -> Result<Option<f64>, String> {
    budget.tick()?;
    let word = |c: Option<&u16>| c.is_some_and(|&c| c == 95 || (c < 128 && (c as u8).is_ascii_alphanumeric()));
    // $name and @name read the database variable store, not the invocation snapshot.
    if text.iter().enumerate().any(|(i, &c)| (c == 36 || c == 64) && word(text.get(i + 1))) {
        return Err("calc-variable".into());
    }
    let expression: Units = replace_pairs(text).into_iter().filter(|&c| !ws(c)).collect();
    let mut tokens: Vec<Units> = Vec::new();
    let mut last: Units = Vec::new();
    for (i, &c) in expression.iter().enumerate() {
        if c == 45 && (i == 0 || operator(expression[i - 1]) || expression[i - 1] == 40) {
            last.push(c)
        } else if operator(c) {
            tokens.push(if last.is_empty() { vec![48] } else { std::mem::take(&mut last) });
            tokens.push(vec![c]);
        } else {
            last.push(c)
        }
    }
    tokens.push(if last.is_empty() { vec![48] } else { last });
    let mut output: Vec<Units> = Vec::new();
    let mut stack: Vec<u16> = Vec::new();
    for token in tokens {
        budget.tick()?;
        if number(&token).is_some() {
            output.push(token);
        } else if token.len() == 1 && operator(token[0]) {
            let t = token[0];
            while let Some(&top) = stack.last() {
                let pops = if t == 33 { precedence(t) < precedence(top) } else { precedence(t) <= precedence(top) };
                if !pops {
                    break;
                }
                output.push(vec![stack.pop().unwrap()]);
            }
            stack.push(t);
        }
    }
    while let Some(t) = stack.pop() {
        output.push(vec![t]);
    }
    if output.is_empty() {
        // ''.split(' ') is [''], which pops two values and pushes none.
        return Ok(Some(0.0));
    }
    let mut values: Vec<Option<f64>> = Vec::new();
    for token in output {
        budget.tick()?;
        if let Some(n) = number(&token) {
            values.push(Some(n));
            continue;
        }
        let b = values.pop().flatten();
        let a = values.pop().flatten();
        if let Some(v) = apply(token[0], a, b)? {
            values.push(Some(v));
        } else if matches!(token[0], 124 | 38) {
            values.push(None);
        }
    }
    Ok(match values.pop() {
        Some(v) => v,
        None => Some(0.0),
    })
}
pub fn js_string(n: f64) -> Units {
    if n == 0.0 {
        vec![48]
    } else {
        number_string(n)
    }
}
// calcString(text); None means the result is undefined and `.toString()` would throw.
pub fn calc_string(text: &[u16], budget: &mut Budget) -> Result<Option<f64>, String> {
    let mut depth: Vec<Units> = vec![Vec::new()];
    for &c in text {
        if c == 40 {
            depth.push(Vec::new());
        } else if c == 41 && depth.len() > 1 {
            let inner = depth.pop().unwrap();
            let value = execute(&inner, budget)?;
            let rendered = match value {
                Some(n) => js_string(n),
                None => units("undefined"),
            };
            depth.last_mut().unwrap().extend(rendered);
        } else {
            depth.last_mut().unwrap().push(c);
        }
    }
    let joined: Units = depth.concat();
    execute(&joined, budget)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn calc(s: &str) -> Result<Option<f64>, String> {
        calc_string(&units(s), &mut Budget::default())
    }
    #[test]
    fn rpn_quirks_follow_the_typescript_tokenizer() {
        assert_eq!(calc("1+2*3"), Ok(Some(7.0)));
        // `parseFloat("0.0") || token === "0"` is false, so the token is dropped and `+` adds to undefined.
        assert!(calc("0.0+2").unwrap().unwrap().is_nan());
        assert_eq!(calc("!0"), Ok(Some(1.0)));
        assert_eq!(calc("0||0||7"), Ok(Some(7.0)));
        assert_eq!(calc("2^3^2"), Ok(Some(64.0)));
        assert_eq!(calc("2^0.5"), Err("pow-inexact".into()));
        assert_eq!(calc("abc"), Ok(Some(0.0)));
        assert_eq!(calc("$x+1"), Err("calc-variable".into()));
        assert!(calc("$ +1").unwrap().unwrap().is_nan());
        assert_eq!(calc("NuLl+2"), Ok(Some(2.0)));
        assert_eq!(calc(""), Ok(Some(0.0)));
    }
    #[test]
    fn parse_float_takes_the_longest_decimal_prefix() {
        for (s, n) in [("1e", 1.0), ("1.e5", 100000.0), (".5x", 0.5), ("-.5e-3x", -0.0005), ("+Infinityx", f64::INFINITY), ("0x10", 0.0), ("5e+", 5.0)] {
            assert_eq!(parse_float(&units(s)), n, "{s}");
        }
        assert!(parse_float(&units(".")).is_nan());
        assert!(parse_float(&units("-")).is_nan());
        assert!(parse_float(&units("infinity")).is_nan());
    }
}
