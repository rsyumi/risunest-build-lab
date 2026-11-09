//! `lib/common/utils.mjs` of markdown-it 14.3.2 over UTF-16 units, plus the JS string semantics the rules rely on.

use crate::entities::{decode_html, Mode};
use crate::error::{RenderError, Result};
use crate::js_engine::UnicodeVersion;
use crate::meter::Meter;
use crate::tables::PUNCT_RANGES;
use risunest_ir::Text;

pub(crate) type Units = Vec<u16>;

pub(crate) fn s(text: &str) -> Units { text.encode_utf16().collect() }

/// `text.units().collect()`, widening ASCII 16 bytes at a time.
pub(crate) fn units_of(text: &Text) -> Units {
    let Some(value) = text.as_unicode() else { return text.units().collect() };
    let bytes = value.as_bytes();
    let mut out = Units::with_capacity(text.len_units());
    let mut at = 0;
    while at < bytes.len() {
        if let Some(chunk) = bytes.get(at..at + 16) {
            if chunk.iter().fold(true, |ascii, byte| ascii & (*byte < 0x80)) {
                out.extend(chunk.iter().map(|byte| *byte as u16));
                at += 16;
                continue;
            }
        }
        let character = value[at..].chars().next().expect("a character at a boundary");
        out.extend_from_slice(character.encode_utf16(&mut [0; 2]));
        at += character.len_utf8();
    }
    out
}

/// `str.charCodeAt(index)`, `None` where JS gives `NaN`.
pub(crate) fn code(src: &[u16], index: usize) -> Option<u16> { src.get(index).copied() }

/// `isSpace` (utils.mjs): tab and space only.
pub(crate) fn is_space(unit: u16) -> bool { unit == 0x09 || unit == 0x20 }
pub(crate) fn is_space_at(src: &[u16], index: usize) -> bool { code(src, index).is_some_and(is_space) }

/// `isWhiteSpace` (utils.mjs): Unicode Zs plus the ASCII controls, without U+2028, U+2029 or U+FEFF.
pub(crate) fn is_white_space(code: u32) -> bool {
    (0x2000..=0x200a).contains(&code) || matches!(code, 0x09..=0x0d | 0x20 | 0xa0 | 0x1680 | 0x202f | 0x205f | 0x3000)
}

/// `isMdAsciiPunct` (utils.mjs).
pub(crate) fn is_md_ascii_punct(code: u32) -> bool { matches!(code, 0x21..=0x2f | 0x3a..=0x40 | 0x5b..=0x60 | 0x7b..=0x7e) }

/// `isPunctCharCode` (utils.mjs): uc.micro `P` or `S` for the code point (a lone unit below 0x10000).
pub(crate) fn is_punct_code(code: u32) -> bool {
    PUNCT_RANGES.binary_search_by(|&(low, high)| {
        if high < code { std::cmp::Ordering::Less } else if low > code { std::cmp::Ordering::Greater } else { std::cmp::Ordering::Equal }
    }).is_ok()
}

/// JS `\s` and the characters `String.prototype.trim` removes.
pub(crate) fn is_js_space(unit: u16) -> bool { risunest_ir::text::is_whitespace(unit) }

/// `String.prototype.trim`.
pub(crate) fn js_trim(src: &[u16]) -> &[u16] {
    let start = src.iter().position(|unit| !is_js_space(*unit)).unwrap_or(src.len());
    let end = src.iter().rposition(|unit| !is_js_space(*unit)).map_or(start, |index| index + 1);
    &src[start..end]
}

/// `asciiTrim` (utils.mjs): space, tab, LF and CR.
pub(crate) fn ascii_trim(src: &[u16]) -> &[u16] {
    let trimmable = |unit: u16| matches!(unit, 0x20 | 0x09 | 0x0a | 0x0d);
    let start = src.iter().position(|unit| !trimmable(*unit)).unwrap_or(src.len());
    let end = src.iter().rposition(|unit| !trimmable(*unit)).map_or(start, |index| index + 1);
    &src[start..end]
}

pub(crate) fn is_ascii_alpha(unit: u16) -> bool { matches!(unit, 0x41..=0x5a | 0x61..=0x7a) }
pub(crate) fn is_ascii_digit(unit: u16) -> bool { (0x30..=0x39).contains(&unit) }
pub(crate) fn is_ascii_alnum(unit: u16) -> bool { is_ascii_alpha(unit) || is_ascii_digit(unit) }
pub(crate) fn is_ascii_hex(unit: u16) -> bool { is_ascii_digit(unit) || matches!(unit, 0x41..=0x46 | 0x61..=0x66) }
pub(crate) fn ascii_lower(unit: u16) -> u16 { if (0x41..=0x5a).contains(&unit) { unit + 0x20 } else { unit } }

/// Case-insensitive ASCII comparison of `src` with a lowercase ASCII `word`, as a regex `i` flag without `u` does
/// for an ASCII pattern (no non-ASCII unit folds onto ASCII there).
pub(crate) fn starts_with_ignore_case(src: &[u16], word: &str) -> bool {
    src.len() >= word.len() && src.iter().zip(word.bytes()).all(|(unit, byte)| ascii_lower(*unit) == byte as u16)
}

pub(crate) fn starts_with(src: &[u16], word: &str) -> bool {
    src.len() >= word.len() && src.iter().zip(word.bytes()).all(|(unit, byte)| *unit == byte as u16)
}

/// The first index of `unit` in `src`, testing 16 units at a time so the test vectorizes.
fn position_unit(src: &[u16], unit: u16) -> Option<usize> {
    let mut offset = 0;
    for chunk in src.chunks(16) {
        if chunk.iter().fold(false, |found, value| found | (*value == unit)) {
            return chunk.iter().position(|value| *value == unit).map(|index| offset + index);
        }
        offset += chunk.len();
    }
    None
}

/// `str.indexOf(needle, from)`.
pub(crate) fn find(src: &[u16], needle: &str, from: usize) -> Option<usize> {
    let mut buffer = [0u16; 32];
    let units = needle.encode_utf16().count();
    if units > buffer.len() { return find_units(src, &s(needle), from); }
    for (slot, unit) in buffer.iter_mut().zip(needle.encode_utf16()) { *slot = unit; }
    find_units(src, &buffer[..units], from)
}

/// `str.indexOf(needle, from)` for a needle in units.
pub(crate) fn find_units(src: &[u16], needle: &[u16], from: usize) -> Option<usize> {
    let Some((&first, rest)) = needle.split_first() else { return Some(from.min(src.len())) };
    let last = src.len().checked_sub(needle.len())?;
    let mut at = from;
    while at <= last {
        at += position_unit(&src[at..=last], first)?;
        if src[at + 1..at + needle.len()] == *rest { return Some(at); }
        at += 1;
    }
    None
}

pub(crate) fn find_unit(src: &[u16], unit: u16, from: usize) -> Option<usize> {
    position_unit(src.get(from..)?, unit).map(|index| index + from)
}

/// `escapeHtml` (utils.mjs).
pub(crate) fn escape_html(src: &[u16], out: &mut Units) {
    for &unit in src {
        match unit {
            0x26 => out.extend_from_slice(&[0x26, 0x61, 0x6d, 0x70, 0x3b]),
            0x3c => out.extend_from_slice(&[0x26, 0x6c, 0x74, 0x3b]),
            0x3e => out.extend_from_slice(&[0x26, 0x67, 0x74, 0x3b]),
            0x22 => out.extend_from_slice(&[0x26, 0x71, 0x75, 0x6f, 0x74, 0x3b]),
            _ => out.push(unit),
        }
    }
}

/// `fromCodePoint` (utils.mjs).
pub(crate) fn from_code_point(code: u32, out: &mut Units) {
    if code > 0xffff {
        let code = code - 0x10000;
        out.push(0xd800 + (code >> 10) as u16);
        out.push(0xdc00 + (code & 0x3ff) as u16);
    } else {
        out.push(code as u16);
    }
}

/// `isValidEntityCode` (utils.mjs).
pub(crate) fn is_valid_entity_code(code: u64) -> bool {
    !((0xd800..=0xdfff).contains(&code)
        || (0xfdd0..=0xfdef).contains(&code)
        || (code & 0xffff) == 0xffff || (code & 0xffff) == 0xfffe
        || code <= 0x08 || code == 0x0b || (0x0e..=0x1f).contains(&code) || (0x7f..=0x9f).contains(&code)
        || code > 0x10ffff)
}

/// `parseInt(digits, radix)` for nonempty ASCII digits of that radix; saturates, since every caller only compares the
/// result with the code point range.
pub(crate) fn parse_int(digits: &[u16], radix: u64) -> u64 {
    digits.iter().fold(0u64, |value, unit| {
        let digit = match *unit { 0x30..=0x39 => unit - 0x30, 0x41..=0x46 => unit - 0x37, _ => unit - 0x57 } as u64;
        value.saturating_mul(radix).saturating_add(digit).min(u64::MAX / 64)
    })
}

/// `&` then `[a-z#][a-z0-9]{1,31};` (case-insensitive), the entity half of `UNESCAPE_ALL_RE` (utils.mjs). Returns
/// the index after `;`.
fn entity_pattern(src: &[u16], at: usize) -> Option<usize> {
    let first = code(src, at + 1)?;
    if !(is_ascii_alpha(first) || first == 0x23) { return None; }
    let run = src[at + 2..].iter().take_while(|unit| is_ascii_alnum(**unit)).count();
    if (1..=31).contains(&run) && code(src, at + 2 + run) == Some(0x3b) { Some(at + 3 + run) } else { None }
}

/// `replaceEntityPattern` (utils.mjs).
fn replace_entity_pattern(matched: &[u16], name: &[u16], out: &mut Units) {
    if name[0] == 0x23 {
        // DIGITAL_ENTITY_TEST_RE: /^#((?:x[a-f0-9]{1,8}|[0-9]{1,8}))$/i
        let hex = name.len() >= 3 && ascii_lower(name[1]) == 0x78 && name.len() <= 10 && name[2..].iter().all(|unit| is_ascii_hex(*unit));
        let decimal = name.len() >= 2 && name.len() <= 9 && name[1..].iter().all(|unit| is_ascii_digit(*unit));
        if hex || decimal {
            let value = if hex { parse_int(&name[2..], 16) } else { parse_int(&name[1..], 10) };
            if is_valid_entity_code(value) { from_code_point(value as u32, out); } else { out.extend_from_slice(matched); }
            return;
        }
    }
    out.extend(decode_html(matched, Mode::Legacy));
}

/// `unescapeAll` (utils.mjs): backslash escapes of ASCII punctuation and HTML entities.
pub(crate) fn unescape_all(src: &[u16], meter: &Meter) -> Result<Units> {
    meter.work(src.len())?;
    if !src.contains(&0x5c) && !src.contains(&0x26) { return Ok(src.to_vec()); }
    let mut out = Vec::with_capacity(src.len());
    let mut index = 0;
    while index < src.len() {
        let unit = src[index];
        if unit == 0x5c {
            if let Some(next) = code(src, index + 1).filter(|next| is_md_ascii_punct(*next as u32)) {
                out.push(next);
                index += 2;
                continue;
            }
        } else if unit == 0x26 {
            if let Some(end) = entity_pattern(src, index) {
                replace_entity_pattern(&src[index..end], &src[index + 1..end - 1], &mut out);
                index = end;
                continue;
            }
        }
        out.push(unit);
        index += 1;
    }
    Ok(out)
}

/// Code points whose `toLowerCase().toUpperCase()` in the oracle's JS engine (Node 24, Unicode 16) differs from Rust's
/// case tables (Unicode 17 adds these lowercase letters); a label holding one declines.
const CASE_DIFFERS: &[u32] = &[
    0xa7cf, 0xa7d3, 0xa7d5, 0x16ebb, 0x16ebc, 0x16ebd, 0x16ebe, 0x16ebf, 0x16ec0, 0x16ec1, 0x16ec2, 0x16ec3, 0x16ec4,
    0x16ec5, 0x16ec6, 0x16ec7, 0x16ec8, 0x16ec9, 0x16eca, 0x16ecb, 0x16ecc, 0x16ecd, 0x16ece, 0x16ecf, 0x16ed0, 0x16ed1,
    0x16ed2, 0x16ed3
];

/// `toLowerCase().toUpperCase()` of one code point in Rust's case tables. A whole label folds the same way one code
/// point at a time: the only context rule, final sigma, picks between two lowercase sigmas that uppercase alike.
fn rust_fold(character: char, out: &mut Units) {
    let mut buffer = [0u16; 2];
    for lower in character.to_lowercase() {
        for upper in lower.to_uppercase() { out.extend_from_slice(upper.encode_utf16(&mut buffer)); }
    }
}

/// Whether Rust's case tables map `character` to anything but itself.
fn has_case_mapping(character: char) -> bool {
    let mut lower = character.to_lowercase();
    let mut upper = character.to_uppercase();
    lower.next() != Some(character) || lower.next().is_some() || upper.next() != Some(character) || upper.next().is_some()
}

/// Rust's case tables are the Unicode 17 ones that the Unicode 16 and 17 folds start from; a toolchain with other tables
/// reproduces neither.
const RUST_CASE_UNICODE_17: bool = matches!(char::UNICODE_VERSION, (17, 0, 0));

/// `toLowerCase().toUpperCase()` of one non-ASCII code point in the engine's case tables, or `None` where the port
/// cannot tell:
/// - Unicode 17: Rust's fold.
/// - Unicode 16: Rust's fold, except the code points in [`CASE_DIFFERS`].
/// - Older: the code point itself when Rust's tables give it no case mapping, since a version only adds mappings.
/// - Newer or not probed: never, since a newer version can give a mapping to a letter Rust's tables leave alone, as
///   Unicode 17 did for U+A7D3, or assign a cased letter where Rust's tables have none.
fn fold_case(character: char, case: Option<UnicodeVersion>, out: &mut Units) -> Option<()> {
    match case {
        Some(UnicodeVersion::Unicode17) if RUST_CASE_UNICODE_17 => rust_fold(character, out),
        Some(UnicodeVersion::Unicode16) if RUST_CASE_UNICODE_17 && !CASE_DIFFERS.contains(&(character as u32)) => rust_fold(character, out),
        Some(UnicodeVersion::Before16) if !has_case_mapping(character) => out.extend_from_slice(character.encode_utf16(&mut [0; 2])),
        _ => return None,
    }
    Some(())
}

/// `normalizeReference` (utils.mjs): trim, collapse `\s+` to one space, then `toLowerCase().toUpperCase()` with the
/// engine's case tables. ASCII folds alike in every engine.
pub(crate) fn normalize_reference(src: &[u16], case: Option<UnicodeVersion>, meter: &Meter) -> Result<Units> {
    meter.work(src.len())?;
    let trimmed = js_trim(src);
    let mut collapsed = Vec::with_capacity(trimmed.len());
    let mut in_space = false;
    for &unit in trimmed {
        if is_js_space(unit) {
            if !in_space { collapsed.push(0x20); }
            in_space = true;
            continue;
        }
        in_space = false;
        collapsed.push(unit);
    }
    let mut out = Vec::with_capacity(collapsed.len());
    for decoded in char::decode_utf16(collapsed.iter().copied()) {
        match decoded {
            Ok(character) if character.is_ascii() => out.push(character.to_ascii_uppercase() as u16),
            Ok(character) => {
                if fold_case(character, case, &mut out).is_none() { return Err(RenderError::Unsupported("markdown-reference-case")); }
            }
            Err(lone) => out.push(lone.unpaired_surrogate()),
        }
    }
    meter.alloc(2 * out.len())?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use risunest_ir::budget::{Budget, Limits};
    use std::collections::BTreeMap;
    use std::sync::{atomic::AtomicBool, Arc};

    const CASES: &[u8] = include_bytes!("../../../../tests/fixtures/rust-core/render/reference-case.cases.json");
    const EXPECTED: &[u8] = include_bytes!("../../../../tests/fixtures/rust-core/render/reference-case.expected.json");

    /// The recorded `normalizeReference` of every code point whose fold is not itself.
    fn recorded_folds() -> BTreeMap<u32, Units> {
        use sha2::{Digest, Sha256};
        let expected: serde_json::Value = serde_json::from_slice(EXPECTED).expect("expected");
        let digest: String = Sha256::digest(CASES).iter().map(|byte| format!("{byte:02x}")).collect();
        assert_eq!(expected["fixtureSha256"], digest.as_str(), "regenerate the expected file");
        let mut changed = BTreeMap::new();
        for entry in expected["cases"][0]["result"].as_array().expect("table") {
            changed.insert(entry[0].as_u64().expect("code") as u32, entry[1].as_str().expect("folded").encode_utf16().collect::<Units>());
        }
        assert!(changed.len() > 1000);
        changed
    }

    fn budget() -> Budget {
        let limits = Limits { max_owned_bytes: 1 << 30, max_json_bytes: 1 << 24, max_text_units: 1 << 24, max_values: 1 << 22, max_depth: 128,
            max_work: 1 << 40 };
        Budget::new(limits, Arc::new(AtomicBool::new(false))).expect("limits")
    }

    /// Every code point outside the surrogates folds as markdown-it's `normalizeReference` folds it in the oracle's JS
    /// engine, except those in [`CASE_DIFFERS`], which decline, and the whitespace the trim removes.
    #[test]
    fn reference_labels_fold_like_the_recorded_case_mapping() {
        let changed = recorded_folds();
        let budget = budget();
        let meter = Meter::new(&budget);
        let (mut differs, mut mismatched) = (Vec::new(), Vec::new());
        for character in (0..=0x10ffffu32).filter_map(char::from_u32) {
            let mut units = [0u16; 2];
            let units = character.encode_utf16(&mut units);
            let js = changed.get(&(character as u32)).cloned().unwrap_or_else(|| units.to_vec());
            match normalize_reference(units, Some(UnicodeVersion::Unicode16), &meter) {
                Ok(folded) => if folded != js { mismatched.push(character as u32) },
                Err(_) => differs.push(character as u32),
            }
        }
        assert!(mismatched.is_empty(), "fold differently from JS: {:x?}", mismatched);
        assert_eq!(differs, CASE_DIFFERS, "the declined code points are exactly the ones Rust folds differently");
    }

    /// The case tables recorded in a Unicode 17 WebView engine (`cbs-case-table-webview.capture.mjs`), as
    /// `toLocaleLowerCase` and `toLocaleUpperCase` of each code point under a locale without tailoring.
    const WEBVIEW_CASE: &[u8] = include_bytes!("../../../../tests/fixtures/rust-core/cbs/case-table-webview.expected.json");

    /// Every code point outside the surrogates folds as `normalizeReference` folds it in the recorded Unicode 17 engine:
    /// its `toLowerCase`, then the `toUpperCase` of each code point of that, which no context rule changes. The whitespace
    /// the trim removes folds to nothing.
    #[test]
    fn reference_labels_fold_like_the_recorded_webview_case_mapping() {
        assert_eq!(char::UNICODE_VERSION, (17, 0, 0), "the Unicode 17 fold is Rust's own");
        let expected: serde_json::Value = serde_json::from_slice(WEBVIEW_CASE).expect("expected");
        let result = &expected["cases"][0]["result"];
        let table = |name: &str| -> BTreeMap<u32, String> {
            result[name].as_array().expect("table").iter()
                .map(|entry| (entry[0].as_u64().expect("code") as u32, entry[1].as_str().expect("mapped").to_owned())).collect()
        };
        let (lower, upper) = (table("lower"), table("upper"));
        assert!(lower.len() > 1000 && upper.len() > 1000);
        let mapped = |table: &BTreeMap<u32, String>, character: char| table.get(&(character as u32)).cloned().unwrap_or_else(|| character.to_string());
        let budget = budget();
        let meter = Meter::new(&budget);
        let (mut declined, mut mismatched) = (Vec::new(), Vec::new());
        for character in (0..=0x10ffffu32).filter_map(char::from_u32) {
            let mut units = [0u16; 2];
            let units = character.encode_utf16(&mut units);
            let js: Units = if units.len() == 1 && is_js_space(units[0]) { Units::new() }
                else { mapped(&lower, character).chars().map(|lowered| mapped(&upper, lowered)).collect::<String>().encode_utf16().collect() };
            match normalize_reference(units, Some(UnicodeVersion::Unicode17), &meter) {
                Ok(folded) => if folded != js { mismatched.push(character as u32) },
                Err(_) => declined.push(character as u32),
            }
        }
        assert!(mismatched.is_empty(), "fold differently from the WebView: {:x?}", &mismatched[..mismatched.len().min(40)]);
        assert!(declined.is_empty(), "declined: {:x?}", &declined[..declined.len().min(40)]);
    }

    /// A stale entry would decline a label the port folds right.
    #[test]
    fn every_case_differs_entry_folds_differently_in_rust_and_js() {
        let changed = recorded_folds();
        for &code in CASE_DIFFERS {
            let character = char::from_u32(code).expect("scalar");
            let mut rust = Units::new();
            rust_fold(character, &mut rust);
            let js = changed.get(&code).cloned().unwrap_or_else(|| character.encode_utf16(&mut [0; 2]).to_vec());
            assert_ne!(rust, js, "U+{code:04X} folds alike in Rust and JS");
        }
    }

    /// An older engine's case mappings are a subset of Unicode 16's, so every code point the recorded engine folds to
    /// something else must have a mapping in Rust's tables, which an older engine declines.
    #[test]
    fn older_engines_decline_every_code_point_the_recorded_engine_folds() {
        let unmapped: Vec<u32> = recorded_folds().into_iter()
            .filter(|(code, folded)| !folded.is_empty() && char::from_u32(*code).is_some_and(|character| !character.is_ascii() && !has_case_mapping(character)))
            .map(|(code, _)| code).collect();
        assert!(unmapped.is_empty(), "folded in JS without a mapping in Rust: {unmapped:x?}");
    }

    /// The case probes on [`crate::JsEngine`] read the recorded engine as Unicode 16 and Rust's tables as newer.
    #[test]
    fn the_case_probes_split_at_unicode_16_and_17() {
        let changed = recorded_folds();
        assert!(char::UNICODE_VERSION.0 >= 17);
        assert!('\u{10d70}'.to_uppercase().eq(['\u{10d50}']), "Rust's tables have Garay");
        assert_eq!(changed.get(&0x10d70), Some(&'\u{10d50}'.encode_utf16(&mut [0; 2]).to_vec()), "the recorded engine has Garay");
        assert!(!'\u{a7d3}'.to_uppercase().eq(['\u{a7d3}']), "Rust's tables give U+A7D3 its capital");
        assert!(!changed.contains_key(&0xa7d3), "the recorded engine leaves U+A7D3 alone");
    }
}
