//! `toASCII` and `toUnicode` of punycode.js 2.3.1 on UTF-16 units. `None` stands for the `RangeError` punycode.js
//! throws, after which markdown-it keeps the hostname as it was.

use crate::utils::Units;

const MAX_INT: i64 = 0x7fff_ffff;
const BASE: i64 = 36;
const T_MIN: i64 = 1;
const T_MAX: i64 = 26;
const SKEW: i64 = 38;
const DAMP: i64 = 700;
const INITIAL_BIAS: i64 = 72;
const INITIAL_N: i64 = 0x80;

fn adapt(delta: i64, points: i64, first: bool) -> i64 {
    let mut delta = if first { delta / DAMP } else { delta >> 1 };
    delta += delta / points;
    let mut k = 0;
    while delta > ((BASE - T_MIN) * T_MAX) >> 1 {
        delta /= BASE - T_MIN;
        k += BASE;
    }
    k + (BASE - T_MIN + 1) * delta / (delta + SKEW)
}

fn threshold(k: i64, bias: i64) -> i64 {
    if k <= bias { T_MIN } else if k >= bias + T_MAX { T_MAX } else { k - bias }
}

fn basic_to_digit(unit: u16) -> i64 {
    match unit {
        0x30..=0x39 => 26 + (unit - 0x30) as i64,
        0x41..=0x5a => (unit - 0x41) as i64,
        0x61..=0x7a => (unit - 0x61) as i64,
        _ => BASE,
    }
}

fn digit_to_basic(digit: i64) -> u16 { (digit + 22 + if digit < 26 { 75 } else { 0 }) as u16 }

/// `ucs2decode`: surrogate pairs join, lone surrogates stay.
fn code_points(units: &[u16]) -> Vec<i64> {
    let mut out = Vec::with_capacity(units.len());
    let mut index = 0;
    while index < units.len() {
        let value = units[index];
        index += 1;
        if (0xd800..=0xdbff).contains(&value) && index < units.len() && (units[index] & 0xfc00) == 0xdc00 {
            out.push((((value & 0x3ff) as i64) << 10) + (units[index] & 0x3ff) as i64 + 0x10000);
            index += 1;
        } else {
            out.push(value as i64);
        }
    }
    out
}

fn decode(input: &[u16]) -> Option<Units> {
    let mut output: Vec<i64> = Vec::new();
    let basic = input.iter().rposition(|unit| *unit == 0x2d).unwrap_or(0);
    for &unit in &input[..basic] {
        if unit >= 0x80 { return None; }
        output.push(unit as i64);
    }
    let (mut i, mut n, mut bias) = (0i64, INITIAL_N, INITIAL_BIAS);
    let mut index = if basic > 0 { basic + 1 } else { 0 };
    while index < input.len() {
        let old = i;
        let mut w = 1i64;
        let mut k = BASE;
        loop {
            let digit = basic_to_digit(*input.get(index)?);
            index += 1;
            if digit >= BASE || digit > (MAX_INT - i) / w { return None; }
            i += digit * w;
            let t = threshold(k, bias);
            if digit < t { break; }
            if w > MAX_INT / (BASE - t) { return None; }
            w *= BASE - t;
            k += BASE;
        }
        let out = output.len() as i64 + 1;
        bias = adapt(i - old, out, old == 0);
        if i / out > MAX_INT - n { return None; }
        n += i / out;
        i %= out;
        output.insert(i as usize, n);
        i += 1;
    }
    // `String.fromCodePoint` throws past U+10FFFF and writes a lone surrogate as it is.
    let mut units = Units::with_capacity(output.len());
    for code in output {
        if code > 0x10ffff { return None; }
        crate::utils::from_code_point(code as u32, &mut units);
    }
    Some(units)
}

fn encode(input: &[u16]) -> Option<Units> {
    let input = code_points(input);
    let mut output: Units = input.iter().filter(|value| **value < 0x80).map(|value| *value as u16).collect();
    let basic = output.len() as i64;
    let mut handled = basic;
    if basic > 0 { output.push(0x2d); }
    let (mut n, mut delta, mut bias) = (INITIAL_N, 0i64, INITIAL_BIAS);
    while handled < input.len() as i64 {
        let m = input.iter().copied().filter(|value| *value >= n).min().unwrap_or(MAX_INT).min(MAX_INT);
        let next = handled + 1;
        if m - n > (MAX_INT - delta) / next { return None; }
        delta += (m - n) * next;
        n = m;
        for &value in &input {
            if value < n {
                delta += 1;
                if delta > MAX_INT { return None; }
            }
            if value == n {
                let mut q = delta;
                let mut k = BASE;
                loop {
                    let t = threshold(k, bias);
                    if q < t { break; }
                    output.push(digit_to_basic(t + (q - t) % (BASE - t)));
                    q = (q - t) / (BASE - t);
                    k += BASE;
                }
                output.push(digit_to_basic(q));
                bias = adapt(delta, next, handled == basic);
                delta = 0;
                handled += 1;
            }
        }
        delta += 1;
        n += 1;
    }
    Some(output)
}

/// `mapDomain` for an mdurl hostname, which never holds the `@` mapDomain splits at: the RFC 3490 separators become
/// `.`, and each label maps on its own.
fn map_domain(domain: &[u16], label: impl Fn(&[u16]) -> Option<Units>) -> Option<Units> {
    let mut out = Units::new();
    let domain: Units = domain.iter().map(|&unit| if matches!(unit, 0x3002 | 0xff0e | 0xff61) { 0x2e } else { unit }).collect();
    for (index, part) in domain.split(|unit| *unit == 0x2e).enumerate() {
        if index > 0 { out.push(0x2e); }
        out.extend(label(part)?);
    }
    Some(out)
}

/// `punycode.toASCII`.
pub(crate) fn to_ascii(domain: &[u16]) -> Option<Units> {
    map_domain(domain, |label| {
        if label.iter().any(|unit| *unit >= 0x80) { encode(label).map(|encoded| [&[0x78, 0x6e, 0x2d, 0x2d][..], &encoded].concat()) } else { Some(label.to_vec()) }
    })
}

/// `punycode.toUnicode`. `toLowerCase` on the label can only make U+212A KELVIN SIGN an ASCII letter; any other
/// non-ASCII unit stays non-ASCII, which `decode` rejects.
pub(crate) fn to_unicode(domain: &[u16]) -> Option<Units> {
    map_domain(domain, |label| {
        if !crate::utils::starts_with(label, "xn--") { return Some(label.to_vec()); }
        let lower: Units = label[4..].iter().map(|&unit| match unit {
            0x41..=0x5a => unit + 0x20,
            0x212a => 0x6b,
            unit => unit,
        }).collect();
        decode(&lower)
    })
}
