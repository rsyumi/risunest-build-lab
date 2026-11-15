//! Script flag parsing.

use crate::script::{equals, find, starts_with, trim, CustomScript, PlanEntry};
use risunest_ir::budget::Budget;
use risunest_ir::stage::StageError;

/// `parseEntry` (`src/ts/process/regexExecutionPlan.ts`): `<...>` metadata, the replacement rewrite and the flags.
pub(crate) fn parse_entry(script: &CustomScript, source_index: usize, budget: &Budget) -> Result<PlanEntry, StageError> {
    let mut flag: Vec<u16> = script.flag.units().collect();
    let mut order = 0.0;
    let mut actions = Vec::new();
    if script.able_flag && flag.contains(&60) {
        let mut items = Vec::new();
        flag = strip_metadata(&flag, |metadata| items.extend(metadata.split(|unit| *unit == 44).map(<[u16]>::to_vec)));
        for item in items {
            let item = trim(&item, budget)?;
            if starts_with(&item, "order ") { order = parse_int(&item[6..]); } else { actions.push(item); }
        }
    }
    let out: Vec<u16> = script.replacement.units().collect();
    // TS also runs `.replace(/{{data}}/g, '$&')`, which puts each match back, so `{{data}}` stays for CBS.
    let mut replacement = replace_all(&out, &ascii_units("$n"), &[10]);
    if replacement.last() == Some(&62) && !actions.iter().any(|action| equals(action, "no_end_nl")) { replacement.push(10); }
    let flags = normalize_flags(script.able_flag, &flag, &actions, &replacement, budget)?;
    let dynamic_pattern = actions.iter().any(|action| equals(action, "cbs"));
    Ok(PlanEntry { source_index, pattern: script.pattern.units().collect(), replacement, out, flags, order, actions, dynamic_pattern })
}

fn ascii_units(value: &str) -> Vec<u16> { value.encode_utf16().collect() }

/// `value.replaceAll(search, replacement)` for a nonempty literal search and a `$`-free replacement.
fn replace_all(value: &[u16], search: &[u16], replacement: &[u16]) -> Vec<u16> {
    let mut output = Vec::with_capacity(value.len());
    let mut start = 0;
    while let Some(index) = find(value, search, start) {
        output.extend_from_slice(&value[start..index]);
        output.extend_from_slice(replacement);
        start = index + search.len();
    }
    output.extend_from_slice(&value[start..]);
    output
}

fn line_terminator(unit: u16) -> bool { matches!(unit, 10 | 13 | 0x2028 | 0x2029) }

/// `flag.replace(/<(.+?)>/g, ...)`: each match calls `visit` with its metadata and is removed.
fn strip_metadata(flag: &[u16], mut visit: impl FnMut(&[u16])) -> Vec<u16> {
    let mut output = Vec::with_capacity(flag.len());
    let mut index = 0;
    while index < flag.len() {
        if flag[index] == 60 {
            // `.+?>`: at least one unit, no line terminator, up to the first `>` after it.
            let mut end = None;
            let mut cursor = index + 1;
            while cursor < flag.len() && !line_terminator(flag[cursor]) {
                if cursor > index + 1 && flag[cursor] == 62 { end = Some(cursor); break; }
                cursor += 1;
            }
            if let Some(end) = end {
                visit(&flag[index + 1..end]);
                index = end + 1;
                continue;
            }
        }
        output.push(flag[index]);
        index += 1;
    }
    output
}

/// `parseInt(value)` without a radix.
pub(crate) fn parse_int(value: &[u16]) -> f64 {
    let space = risunest_ir::text::is_whitespace;
    let mut rest = &value[value.iter().position(|unit| !space(*unit)).unwrap_or(value.len())..];
    let mut sign = 1.0;
    if let Some(&first) = rest.first() {
        if first == 45 { sign = -1.0; }
        if first == 43 || first == 45 { rest = &rest[1..]; }
    }
    let mut radix = 10;
    if rest.len() >= 2 && rest[0] == 48 && (rest[1] == 120 || rest[1] == 88) { rest = &rest[2..]; radix = 16; }
    let digit = |unit: u16| -> Option<u32> { char::from_u32(unit as u32)?.to_digit(radix) };
    let digits: Vec<u32> = rest.iter().map_while(|unit| digit(*unit)).collect();
    if digits.is_empty() { return f64::NAN; }
    let magnitude = if radix == 10 {
        let text: String = digits.iter().map(|digit| char::from_digit(*digit, 10).expect("decimal digit")).collect();
        text.parse::<f64>().expect("decimal digits")
    } else {
        // Exact for power-of-two radixes: 32 leading hex digits fit in u128 and the rest only add a sticky bit.
        let significant: Vec<u32> = digits.iter().copied().skip_while(|digit| *digit == 0).collect();
        let head = significant.len().min(32);
        let mut mantissa = significant[..head].iter().fold(0u128, |value, digit| (value << 4) | *digit as u128);
        if significant[head..].iter().any(|digit| *digit != 0) { mantissa |= 1; }
        let exponent = 4 * (significant.len() - head) as i32;
        (mantissa as f64) * 2f64.powi(exponent)
    };
    sign * magnitude
}

/// `normalizeFlags`.
fn normalize_flags(able_flag: bool, flag: &[u16], actions: &[Vec<u16>], replacement: &[u16], budget: &Budget) -> Result<String, StageError> {
    let mut flags: Vec<u16> = if able_flag && !flag.is_empty() { flag.to_vec() } else { vec![103] };
    let moves = starts_with(replacement, "@@move_top") || starts_with(replacement, "@@move_bottom")
        || actions.iter().any(|action| equals(action, "move_top") || equals(action, "move_bottom"));
    if moves {
        if let Some(index) = flags.iter().position(|unit| *unit == 103) { flags.remove(index); }
    }
    let mut output = String::new();
    for unit in trim(&flags, budget)? {
        let Some(flag) = char::from_u32(unit as u32).filter(|flag| "dgimsuvy".contains(*flag)) else { continue };
        if !output.contains(flag) { output.push(flag); }
    }
    if output.is_empty() { output.push('u'); }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn units(value: &str) -> Vec<u16> { value.encode_utf16().collect() }

    #[test]
    fn parse_int_follows_javascript() {
        for (input, expected) in [("5", 5.0), ("  -3x", -3.0), ("+7", 7.0), ("0x1F", 31.0), ("0X", f64::NAN), ("1.5", 1.0),
            ("1e3", 1.0), ("", f64::NAN), ("abc", f64::NAN), ("\u{a0}\n12", 12.0), ("-0", -0.0),
            ("123456789012345678901234567890", 1.2345678901234568e29)] {
            let actual = parse_int(&units(input));
            assert!(actual == expected || (actual.is_nan() && expected.is_nan()), "{input:?}: {actual}");
            assert_eq!(actual.is_sign_negative(), expected.is_sign_negative() && !expected.is_nan(), "{input:?}");
        }
        assert_eq!(parse_int(&units("0x10000000000000000000000000000000001")), 2f64.powi(136));
    }

    #[test]
    fn metadata_matches_lazily_and_stops_at_line_terminators() {
        let mut seen = Vec::new();
        let rest = strip_metadata(&units("a<b>c<>>d<e\nf>g<h"), |metadata| seen.push(String::from_utf16_lossy(metadata)));
        assert_eq!((seen, String::from_utf16_lossy(&rest)), (vec!["b".to_string(), ">".to_string()], "acd<e\nf>g<h".to_string()));
    }
}
