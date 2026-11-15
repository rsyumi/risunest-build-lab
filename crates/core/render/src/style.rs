//! The string steps around a `<risu-style>` element that `trimMarkdown` (`src/ts/parser/parser.svelte.ts`) swaps for a
//! `<style>` after DOMPurify: `decodeStyleContent` turns the element's hex text back into CSS, the WebView runs CBS and
//! scopes the CSS, and the scoped CSS is escaped before it becomes the `<style>` text.

use crate::error::Result;
use crate::meter::Meter;
use crate::utils::{find_unit, is_ascii_alnum, is_js_space, starts_with_ignore_case, units_of, Units};
use risunest_ir::{Budget, Text};

/// `parseInt(pair, 16)` for a string of at most two units: leading whitespace, a sign, a `0x` prefix, then the hex
/// digits up to the first other unit, or `None` for NaN.
fn parse_hex_pair(pair: &[u16]) -> Option<i32> {
    let mut rest = pair;
    while let Some((&first, tail)) = rest.split_first() {
        if !is_js_space(first) { break; }
        rest = tail;
    }
    let mut sign = 1;
    if let Some((&first, tail)) = rest.split_first() {
        if first == 0x2d || first == 0x2b {
            sign = if first == 0x2d { -1 } else { 1 };
            rest = tail;
        }
    }
    if rest.len() >= 2 && rest[0] == 0x30 && (rest[1] == 0x78 || rest[1] == 0x58) { rest = &rest[2..]; }
    let digit = |unit: u16| char::from_u32(unit as u32).and_then(|c| c.to_digit(16));
    let digits: Vec<u32> = rest.iter().map_while(|&unit| digit(unit)).collect();
    if digits.is_empty() { return None; }
    Some(sign * digits.iter().fold(0, |value, digit| value * 16 + *digit as i32))
}

/// `Buffer.from(hex, 'hex').toString('utf-8')` with the `buffer` 6.0.3 package the app installs as `globalThis.Buffer`
/// (`src/ts/polyfill.ts`), as `decodeStyleContent` reads a `<risu-style>` body. `hexWrite` parses each pair of units
/// with `parseInt(pair, 16)` and stops at the first NaN, so `'1g'` is the byte 1; `utf8Slice` writes U+FFFD for each
/// byte that starts no valid sequence and moves on by one byte.
pub fn decode_style_hex(hex: &Text, budget: &Budget) -> Result<Text> {
    let meter = Meter::new(budget);
    let src: Units = units_of(&hex);
    meter.alloc(3 * src.len())?;
    meter.work(src.len())?;
    let mut bytes = Vec::with_capacity(src.len() / 2);
    for pair in src.chunks_exact(2) {
        let Some(value) = parse_hex_pair(pair) else { break };
        // A `Uint8Array` element stores its value modulo 256.
        bytes.push(value.rem_euclid(256) as u8);
    }
    let mut out = Units::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        let first = bytes[index] as u32;
        let mut length = if first > 0xef { 4 } else if first > 0xdf { 3 } else if first > 0xbf { 2 } else { 1 };
        let continuation = |at: usize| bytes[at] & 0xc0 == 0x80;
        let tail = |at: usize| (bytes[at] & 0x3f) as u32;
        let mut code = None;
        if index + length <= bytes.len() {
            code = match length {
                1 => (first < 0x80).then_some(first),
                2 if continuation(index + 1) => Some((first & 0x1f) << 6 | tail(index + 1)).filter(|code| *code > 0x7f),
                3 if continuation(index + 1) && continuation(index + 2) => Some((first & 0xf) << 12 | tail(index + 1) << 6 | tail(index + 2))
                    .filter(|code| *code > 0x7ff && !(0xd800..=0xdfff).contains(code)),
                4 if continuation(index + 1) && continuation(index + 2) && continuation(index + 3) => {
                    Some((first & 0xf) << 18 | tail(index + 1) << 12 | tail(index + 2) << 6 | tail(index + 3)).filter(|code| (0x10000..0x110000).contains(code))
                }
                _ => None,
            };
        }
        match code {
            None => {
                out.push(0xfffd);
                length = 1;
            }
            Some(code) if code > 0xffff => {
                let code = code - 0x10000;
                out.push(0xd800 | (code >> 10) as u16);
                out.push(0xdc00 | (code & 0x3ff) as u16);
            }
            Some(code) => out.push(code as u16),
        }
        index += length;
    }
    meter.text(out.len())?;
    meter.finish()?;
    Ok(Text::from_units(&out, budget)?)
}

/// `css.replaceAll(/<\/(?=style)/gi, '<\\/')`: the scoped CSS of a `<style>` element with every `</` before `style` in
/// any ASCII case written `<\/`, so the text cannot close the element early once the HTML is parsed again.
pub fn escape_style_close(css: &Text, budget: &Budget) -> Result<Text> {
    let meter = Meter::new(budget);
    let src: Units = units_of(&css);
    meter.alloc(4 * src.len())?;
    meter.work(src.len())?;
    let mut out = Units::with_capacity(src.len());
    for (index, &unit) in src.iter().enumerate() {
        out.push(unit);
        if unit == 0x3c && src.get(index + 1) == Some(&0x2f) && starts_with_ignore_case(&src[index + 2..], "style") { out.push(0x5c); }
    }
    meter.text(out.len())?;
    meter.finish()?;
    Ok(Text::from_units(&out, budget)?)
}

/// `hasCbsSyntax` (`src/ts/parser/cbsSyntax.ts`), `/\{[{#]|<(?:user|char|bot)>/i`: without it, `risuChatParser`
/// returns its input, so the CBS pass on a decoded style changes nothing.
pub fn has_cbs_syntax(text: &Text) -> bool {
    let src: Units = units_of(&text);
    src.iter().enumerate().any(|(index, &unit)| match unit {
        0x7b => matches!(src.get(index + 1), Some(0x7b | 0x23)),
        0x3c => ["user>", "char>", "bot>"].iter().any(|word| starts_with_ignore_case(&src[index + 1..], word)),
        _ => false,
    })
}

/// `/<risu-style\b/i.test(data)`, which decides whether the render prepares the CBS history for its styles.
pub fn has_risu_style(text: &Text) -> bool {
    let src: Units = units_of(&text);
    const TAG: &str = "<risu-style";
    let mut from = 0;
    while let Some(index) = find_unit(&src, 0x3c, from) {
        if starts_with_ignore_case(&src[index..], TAG) && !src.get(index + TAG.len()).is_some_and(|&unit| is_ascii_alnum(unit) || unit == 0x5f) {
            return true;
        }
        from = index + 1;
    }
    false
}
