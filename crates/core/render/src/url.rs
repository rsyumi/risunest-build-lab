//! markdown-it's link helpers (`lib/index.mjs`: `validateLink`, `normalizeLink`, `normalizeLinkText`) and the parts
//! of mdurl 2.1.0 they call (`parse` with `slashesDenoteHost`, `format`, `encode`, `decode`), with hostnames recoded by
//! [`crate::punycode`].

use crate::error::Result;
use crate::meter::Meter;
use crate::utils::{ascii_lower, find_unit, is_ascii_alnum, is_ascii_hex, js_trim, s, starts_with, Units};

/// mdurl `Url`; an empty field is the `null` or `''` that `format` skips.
#[derive(Default)]
struct Url {
    protocol: Units,
    slashes: bool,
    auth: Units,
    port: Units,
    hostname: Units,
    hash: Units,
    search: Units,
    pathname: Units,
}

const NON_HOST: &[u16] = &[
    0x25, 0x2f, 0x3f, 0x3b, 0x23, // % / ? ; #
    0x27, 0x7b, 0x7d, 0x7c, 0x5c, 0x5e, 0x60, // ' { } | \ ^ `
    0x3c, 0x3e, 0x22, 0x60, 0x20, 0x0d, 0x0a, 0x09, // < > " ` space \r \n \t
];

fn hostname_part_char(unit: u16) -> bool { is_ascii_alnum(unit) || matches!(unit, 0x2b | 0x5f | 0x2d) }
fn is_line_terminator(unit: u16) -> bool { matches!(unit, 0x0a | 0x0d | 0x2028 | 0x2029) }

fn first_of(src: &[u16], units: &[u16]) -> Option<usize> { src.iter().position(|unit| units.contains(unit)) }

/// mdurl `Url.prototype.parse(url, true)` (parse.mjs).
fn parse(url: &[u16]) -> Url {
    let mut out = Url::default();
    let mut rest: Units = js_trim(url).to_vec();
    // protocolPattern /^([a-z0-9.+-]+:)/i
    let run = rest.iter().take_while(|unit| is_ascii_alnum(**unit) || matches!(**unit, 0x2e | 0x2b | 0x2d)).count();
    if run > 0 && rest.get(run) == Some(&0x3a) {
        out.protocol = rest[..=run].to_vec();
        rest.drain(..=run);
    }
    let proto = &out.protocol;
    let hostless = *proto == s("javascript:");
    let slashed = |value: &[u16]| ["http:", "https:", "ftp:", "gopher:", "file:"].iter().any(|name| value == s(name).as_slice());
    let slashes = starts_with(&rest, "//");
    if slashes && !hostless {
        rest.drain(..2);
        out.slashes = true;
    }
    if !hostless && (slashes || (!proto.is_empty() && !slashed(proto))) {
        let host_end = first_of(&rest, &[0x2f, 0x3f, 0x23]);
        let at_sign = rest[..host_end.unwrap_or(rest.len())].iter().rposition(|unit| *unit == 0x40);
        if let Some(at_sign) = at_sign {
            out.auth = rest[..at_sign].to_vec();
            rest.drain(..=at_sign);
        }
        let mut host_end = first_of(&rest, NON_HOST).unwrap_or(rest.len());
        if host_end > 0 && rest[host_end - 1] == 0x3a { host_end -= 1; }
        let mut host: Units = rest.drain(..host_end).collect();
        // parseHost: portPattern /:[0-9]*$/
        let digits = host.iter().rev().take_while(|unit| (0x30..=0x39).contains(*unit)).count();
        if host.len() > digits && host[host.len() - digits - 1] == 0x3a {
            if digits > 0 { out.port = host[host.len() - digits..].to_vec(); }
            host.truncate(host.len() - digits - 1);
        }
        out.hostname = host;
        let ipv6 = out.hostname.first() == Some(&0x5b) && out.hostname.last() == Some(&0x5d);
        if !ipv6 {
            let parts: Vec<Units> = out.hostname.split(|unit| *unit == 0x2e).map(|part| part.to_vec()).collect();
            for (index, part) in parts.iter().enumerate() {
                if part.is_empty() { continue; }
                let valid = |value: &[u16]| value.len() <= 63 && value.iter().all(|unit| hostname_part_char(*unit));
                if valid(part) { continue; }
                let replaced: Units = part.iter().map(|unit| if *unit > 127 { 0x78 } else { *unit }).collect();
                if valid(&replaced) { continue; }
                let mut valid_parts: Vec<Units> = parts[..index].to_vec();
                let mut not_host: Vec<Units> = parts[index + 1..].to_vec();
                // hostnamePartStart /^([+a-z0-9A-Z_-]{0,63})(.*)$/: `.` stops at line terminators.
                let lead = part.iter().take(63).take_while(|unit| hostname_part_char(**unit)).count();
                if !part[lead..].iter().any(|unit| is_line_terminator(*unit)) {
                    valid_parts.push(part[..lead].to_vec());
                    not_host.insert(0, part[lead..].to_vec());
                }
                if !not_host.is_empty() {
                    let mut joined = not_host.join(&0x2e);
                    joined.extend_from_slice(&rest);
                    rest = joined;
                }
                out.hostname = valid_parts.join(&0x2e);
                break;
            }
        }
        if out.hostname.len() > 255 { out.hostname.clear(); }
        if ipv6 { out.hostname = out.hostname[1..out.hostname.len() - 1].to_vec(); }
    }
    if let Some(hash) = find_unit(&rest, 0x23, 0) {
        out.hash = rest.split_off(hash);
    }
    if let Some(query) = find_unit(&rest, 0x3f, 0) {
        out.search = rest.split_off(query);
    }
    if !rest.is_empty() { out.pathname = rest; }
    out
}

/// mdurl `format` (format.mjs).
fn format(url: &Url) -> Units {
    let mut out = url.protocol.clone();
    if url.slashes { out.extend_from_slice(&[0x2f, 0x2f]); }
    if !url.auth.is_empty() {
        out.extend_from_slice(&url.auth);
        out.push(0x40);
    }
    if url.hostname.contains(&0x3a) {
        out.push(0x5b);
        out.extend_from_slice(&url.hostname);
        out.push(0x5d);
    } else {
        out.extend_from_slice(&url.hostname);
    }
    if !url.port.is_empty() {
        out.push(0x3a);
        out.extend_from_slice(&url.port);
    }
    out.extend_from_slice(&url.pathname);
    out.extend_from_slice(&url.search);
    out.extend_from_slice(&url.hash);
    out
}

fn push_hex_byte(byte: u8, out: &mut Units) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    out.push(0x25);
    out.push(HEX[(byte >> 4) as usize] as u16);
    out.push(HEX[(byte & 15) as usize] as u16);
}

/// `encodeURIComponent` of one character.
fn encode_component(character: char, out: &mut Units) {
    let mut bytes = [0u8; 4];
    for byte in character.encode_utf8(&mut bytes).bytes() { push_hex_byte(byte, out); }
}

/// mdurl `encode(string)` with the default exclusions `;/?:@&=+$,-_.!~*'()#` and `keepEscaped` (encode.mjs).
fn encode(src: &[u16]) -> Units {
    const KEEP: &[u8] = b";/?:@&=+$,-_.!~*'()#";
    let mut out = Vec::with_capacity(src.len());
    let mut index = 0;
    while index < src.len() {
        let unit = src[index];
        if unit == 0x25 && index + 2 < src.len() && is_ascii_hex(src[index + 1]) && is_ascii_hex(src[index + 2]) {
            out.extend_from_slice(&src[index..index + 3]);
            index += 3;
            continue;
        }
        if unit < 128 {
            if is_ascii_alnum(unit) || KEEP.contains(&(unit as u8)) { out.push(unit); } else { push_hex_byte(unit as u8, &mut out); }
        } else if (0xd800..=0xdfff).contains(&unit) {
            let pair = (unit <= 0xdbff).then(|| src.get(index + 1).copied()).flatten().filter(|next| (0xdc00..=0xdfff).contains(next));
            match pair {
                Some(low) => {
                    let code = 0x10000 + (((unit as u32) - 0xd800) << 10) + (low as u32 - 0xdc00);
                    encode_component(char::from_u32(code).unwrap_or('\u{fffd}'), &mut out);
                    index += 1;
                }
                None => out.extend(s("%EF%BF%BD")),
            }
        } else {
            encode_component(char::from_u32(unit as u32).unwrap_or('\u{fffd}'), &mut out);
        }
        index += 1;
    }
    out
}

fn hex_value(unit: u16) -> u32 {
    match unit { 0x30..=0x39 => (unit - 0x30) as u32, 0x41..=0x46 => (unit - 0x37) as u32, _ => (unit - 0x57) as u32 }
}

/// mdurl `decode(string, ';/?:@&=+$,#' + '%')` (decode.mjs), as `normalizeLinkText` calls it.
fn decode(src: &[u16]) -> Units {
    const KEEP_ENCODED: &[u8] = b";/?:@&=+$,#%";
    let is_escape = |at: usize| src.get(at) == Some(&0x25) && src.get(at + 1).is_some_and(|unit| is_ascii_hex(*unit))
        && src.get(at + 2).is_some_and(|unit| is_ascii_hex(*unit));
    let mut out = Vec::with_capacity(src.len());
    let mut index = 0;
    while index < src.len() {
        if !is_escape(index) {
            out.push(src[index]);
            index += 1;
            continue;
        }
        let start = index;
        while is_escape(index) { index += 3; }
        let seq = &src[start..index];
        let byte = |at: usize| hex_value(seq[at]) << 4 | hex_value(seq[at + 1]);
        let length = seq.len();
        let mut i = 0;
        while i < length {
            let b1 = byte(i + 1);
            if b1 < 0x80 {
                if KEEP_ENCODED.contains(&(b1 as u8)) { push_hex_byte(b1 as u8, &mut out); } else { out.push(b1 as u16); }
                i += 3;
                continue;
            }
            if (b1 & 0xe0) == 0xc0 && i + 3 < length {
                let b2 = byte(i + 4);
                if (b2 & 0xc0) == 0x80 {
                    let chr = ((b1 << 6) & 0x7c0) | (b2 & 0x3f);
                    if chr < 0x80 { out.extend_from_slice(&[0xfffd, 0xfffd]); } else { out.push(chr as u16); }
                    i += 6;
                    continue;
                }
            }
            if (b1 & 0xf0) == 0xe0 && i + 6 < length {
                let (b2, b3) = (byte(i + 4), byte(i + 7));
                if (b2 & 0xc0) == 0x80 && (b3 & 0xc0) == 0x80 {
                    let chr = ((b1 << 12) & 0xf000) | ((b2 << 6) & 0xfc0) | (b3 & 0x3f);
                    if chr < 0x800 || (0xd800..=0xdfff).contains(&chr) { out.extend_from_slice(&[0xfffd; 3]); } else { out.push(chr as u16); }
                    i += 9;
                    continue;
                }
            }
            if (b1 & 0xf8) == 0xf0 && i + 9 < length {
                let (b2, b3, b4) = (byte(i + 4), byte(i + 7), byte(i + 10));
                if (b2 & 0xc0) == 0x80 && (b3 & 0xc0) == 0x80 && (b4 & 0xc0) == 0x80 {
                    let chr = ((b1 << 18) & 0x1c0000) | ((b2 << 12) & 0x3f000) | ((b3 << 6) & 0xfc0) | (b4 & 0x3f);
                    if !(0x10000..=0x10ffff).contains(&chr) {
                        out.extend_from_slice(&[0xfffd; 4]);
                    } else {
                        let chr = chr - 0x10000;
                        out.push(0xd800 + (chr >> 10) as u16);
                        out.push(0xdc00 + (chr & 0x3ff) as u16);
                    }
                    i += 12;
                    continue;
                }
            }
            out.push(0xfffd);
            i += 3;
        }
    }
    out
}

fn recodes_hostname(url: &Url) -> bool {
    !url.hostname.is_empty() && (url.protocol.is_empty() || ["http:", "https:", "mailto:"].iter().any(|name| url.protocol == s(name)))
}

/// `validateLink` (index.mjs). Only ASCII letters matter for the protocol tests: no non-ASCII character
/// lowercases into one of `vbscript`, `javascript`, `file` or `data` followed by `:`.
pub(crate) fn validate_link(url: &[u16]) -> bool {
    let lower: Units = js_trim(url).iter().map(|unit| ascii_lower(*unit)).collect();
    let bad = ["vbscript:", "javascript:", "file:", "data:"].iter().any(|proto| starts_with(&lower, proto));
    !bad || ["data:image/gif;", "data:image/png;", "data:image/jpeg;", "data:image/webp;"].iter().any(|proto| starts_with(&lower, proto))
}

/// `normalizeLink` (index.mjs).
pub(crate) fn normalize_link(url: &[u16], meter: &Meter) -> Result<Units> {
    meter.work(url.len())?;
    let mut parsed = parse(url);
    if recodes_hostname(&parsed) {
        if let Some(hostname) = crate::punycode::to_ascii(&parsed.hostname) { parsed.hostname = hostname; }
    }
    Ok(encode(&format(&parsed)))
}

/// `normalizeLinkText` (index.mjs).
pub(crate) fn normalize_link_text(url: &[u16], meter: &Meter) -> Result<Units> {
    meter.work(url.len())?;
    let mut parsed = parse(url);
    if recodes_hostname(&parsed) {
        if let Some(hostname) = crate::punycode::to_unicode(&parsed.hostname) { parsed.hostname = hostname; }
    }
    Ok(decode(&format(&parsed)))
}
