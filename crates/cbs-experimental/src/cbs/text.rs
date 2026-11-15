// UTF-16 string, case and byte codecs with the JavaScript behavior the CBS callbacks observe.
use super::{ws, Units};

pub(super) fn trim_start(s: &[u16]) -> &[u16] {
    &s[s.iter().position(|&c| !ws(c)).unwrap_or(s.len())..]
}
pub(super) fn trim_end(s: &[u16]) -> &[u16] {
    &s[..s.iter().rposition(|&c| !ws(c)).map(|n| n + 1).unwrap_or(0)]
}
pub(super) fn find(s: &[u16], needle: &[u16]) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    s.windows(needle.len()).position(|w| w == needle)
}
pub(super) fn rfind(s: &[u16], needle: &[u16]) -> Option<usize> {
    if needle.len() > s.len() {
        return None;
    }
    (0..=s.len() - needle.len()).rev().find(|&i| &s[i..i + needle.len()] == needle)
}

enum Piece {
    Text(String),
    Lone(u16),
}
// Lone surrogates are neither cased nor case-ignorable, so mapping the well-formed runs
// between them separately keeps the Final_Sigma context of String.prototype.toLowerCase.
fn pieces(s: &[u16]) -> Vec<Piece> {
    let mut out = Vec::new();
    let mut run = String::new();
    for c in char::decode_utf16(s.iter().copied()) {
        match c {
            Ok(c) => run.push(c),
            Err(e) => {
                if !run.is_empty() {
                    out.push(Piece::Text(std::mem::take(&mut run)));
                }
                out.push(Piece::Lone(e.unpaired_surrogate()));
            }
        }
    }
    if !run.is_empty() {
        out.push(Piece::Text(run));
    }
    out
}
fn map_case(s: &[u16], upper: bool) -> Units {
    if s.iter().all(|&c| c < 128) {
        return s
            .iter()
            .map(|&c| {
                let c = c as u8;
                u16::from(if upper { c.to_ascii_uppercase() } else { c.to_ascii_lowercase() })
            })
            .collect();
    }
    let mut out = Vec::with_capacity(s.len());
    for piece in pieces(s) {
        match piece {
            Piece::Text(t) => out.extend((if upper { t.to_uppercase() } else { t.to_lowercase() }).encode_utf16()),
            Piece::Lone(u) => out.push(u),
        }
    }
    out
}
pub(super) fn lower(s: &[u16]) -> Units {
    map_case(s, false)
}
pub(super) fn upper(s: &[u16]) -> Units {
    map_case(s, true)
}

pub(super) use risunest_ir::text::unicode16_case_differs as case_differs;
// Code-point boundaries of the string iterator: a surrogate pair is one step, a lone surrogate another.
pub(super) fn code_points(s: &[u16]) -> Vec<&[u16]> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        let n = if (0xd800..0xdc00).contains(&s[i]) && s.get(i + 1).is_some_and(|c| (0xdc00..0xe000).contains(c)) { 2 } else { 1 };
        out.push(&s[i..i + n]);
        i += n;
    }
    out
}

// TextEncoder.encode: lone surrogates become U+FFFD.
pub(super) fn utf8_encode(s: &[u16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut buf = [0u8; 4];
    for c in char::decode_utf16(s.iter().copied()) {
        out.extend_from_slice(c.unwrap_or('\u{fffd}').encode_utf8(&mut buf).as_bytes());
    }
    out
}
// TextDecoder.decode (utf-8, not fatal, BOM removed): maximal subparts become U+FFFD.
pub(super) fn utf8_decode(bytes: &[u8]) -> Units {
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    String::from_utf8_lossy(bytes).encode_utf16().collect()
}
// utf8Slice of the npm buffer package: every byte of a rejected sequence is one U+FFFD.
pub(super) fn buffer_utf8_slice(buf: &[u8]) -> Units {
    let mut out = Vec::with_capacity(buf.len());
    let mut i = 0;
    while i < buf.len() {
        let first = buf[i];
        let mut width = if first > 0xef { 4 } else if first > 0xdf { 3 } else if first > 0xbf { 2 } else { 1 };
        let mut point: Option<u32> = None;
        if i + width <= buf.len() {
            let b = |n: usize| u32::from(buf[i + n]);
            let cont = |n: usize| buf[i + n] & 0xc0 == 0x80;
            match width {
                1 => {
                    if first < 0x80 {
                        point = Some(u32::from(first))
                    }
                }
                2 => {
                    if cont(1) {
                        let t = (u32::from(first) & 0x1f) << 6 | (b(1) & 0x3f);
                        if t > 0x7f {
                            point = Some(t)
                        }
                    }
                }
                3 => {
                    if cont(1) && cont(2) {
                        let t = (u32::from(first) & 0xf) << 12 | (b(1) & 0x3f) << 6 | (b(2) & 0x3f);
                        if t > 0x7ff && !(0xd800..=0xdfff).contains(&t) {
                            point = Some(t)
                        }
                    }
                }
                _ => {
                    if cont(1) && cont(2) && cont(3) {
                        let t = (u32::from(first) & 0xf) << 18 | (b(1) & 0x3f) << 12 | (b(2) & 0x3f) << 6 | (b(3) & 0x3f);
                        if t > 0xffff && t < 0x110000 {
                            point = Some(t)
                        }
                    }
                }
            }
        }
        match point {
            None => {
                out.push(0xfffd);
                width = 1;
            }
            Some(p) if p > 0xffff => {
                let p = p - 0x10000;
                out.push((p >> 10 & 0x3ff | 0xd800) as u16);
                out.push((p & 0x3ff | 0xdc00) as u16);
            }
            Some(p) => out.push(p as u16),
        }
        i += width;
    }
    out
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
// base64-js fromByteArray, as Buffer#toString('base64') uses it.
pub(super) fn base64_encode(bytes: &[u8]) -> Units {
    let mut out = Vec::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = u32::from(chunk[0]) << 16 | u32::from(*chunk.get(1).unwrap_or(&0)) << 8 | u32::from(*chunk.get(2).unwrap_or(&0));
        let digits = [n >> 18 & 63, n >> 12 & 63, n >> 6 & 63, n & 63];
        for (i, d) in digits.iter().enumerate() {
            out.push(if i <= chunk.len() { u16::from(ALPHABET[*d as usize]) } else { 61 });
        }
    }
    out
}
fn base64_value(c: u16) -> Option<u32> {
    Some(match c {
        65..=90 => u32::from(c - 65),
        97..=122 => u32::from(c - 97 + 26),
        48..=57 => u32::from(c - 48 + 52),
        43 | 45 => 62,
        47 | 95 => 63,
        _ => return None,
    })
}
// Buffer.from(string, 'base64') of the npm buffer package: input stops at the first '=',
// other characters outside the URL-safe and standard alphabets are dropped, and a final
// single character is ignored.
pub(super) fn base64_decode(s: &[u16]) -> Vec<u8> {
    let end = s.iter().position(|&c| c == 61).unwrap_or(s.len());
    let values: Vec<u32> = s[..end].iter().filter_map(|&c| base64_value(c)).collect();
    if values.len() < 2 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(values.len() * 3 / 4);
    let whole = values.len() - values.len() % 4;
    for g in values[..whole].chunks(4) {
        let n = g[0] << 18 | g[1] << 12 | g[2] << 6 | g[3];
        out.extend_from_slice(&[(n >> 16) as u8, (n >> 8) as u8, n as u8]);
    }
    let tail = &values[whole..];
    match tail.len() {
        2 => out.push(((tail[0] << 2) | (tail[1] >> 4)) as u8),
        3 => {
            let n = (tail[0] << 10) | (tail[1] << 4) | (tail[2] >> 2);
            out.extend_from_slice(&[(n >> 8) as u8, n as u8]);
        }
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::units;
    use super::*;

    #[test]
    fn case_mapping_keeps_lone_surrogates_and_final_sigma_boundaries() {
        assert_eq!(lower(&units("ΑΣ")), units("ας"));
        assert_eq!(lower(&units("ΑΣΑ")), units("ασα"));
        let mut a = units("ΑΣ");
        a.push(0xd800);
        let mut expected = units("ας");
        expected.push(0xd800);
        assert_eq!(lower(&a), expected);
        let mut b = vec![0xd800];
        b.extend(units("Σb"));
        let mut expected = vec![0xd800];
        expected.extend(units("σb"));
        assert_eq!(lower(&b), expected);
        assert_eq!(upper(&units("ßŉ")), units("SSʼN"));
        assert_eq!(lower(&units("İ")), units("i\u{307}"));
    }

    #[test]
    fn buffer_codecs_match_the_npm_buffer_package() {
        assert_eq!(base64_decode(&units("YWJj")), b"abc");
        assert_eq!(base64_decode(&units("YWJjZ")), b"abc");
        assert_eq!(base64_decode(&units("YW=Jj")), b"a");
        assert_eq!(base64_decode(&units("Y")), b"");
        assert_eq!(base64_decode(&units(" Y W\nJ j ")), b"abc");
        assert_eq!(base64_decode(&units("_-8")), vec![0xff, 0xef]);
        assert_eq!(base64_encode(b"ab"), units("YWI="));
        assert_eq!(base64_encode(b""), units(""));
        assert_eq!(buffer_utf8_slice(&[0xe2, 0x82]), vec![0xfffd, 0xfffd]);
        assert_eq!(buffer_utf8_slice(&[0xc0, 0x80, 0xef, 0xbb, 0xbf]), vec![0xfffd, 0xfffd, 0xfeff]);
        assert_eq!(buffer_utf8_slice(&[0xf0, 0x9f, 0x98, 0x80]), units("😀"));
        assert_eq!(utf8_decode(&[0xef, 0xbb, 0xbf, 0xe2, 0x82, 0x41]), vec![0xfffd, 0x41]);
        assert_eq!(utf8_encode(&[0xd800, 0x41]), vec![0xef, 0xbf, 0xbd, 0x41]);
    }
}
