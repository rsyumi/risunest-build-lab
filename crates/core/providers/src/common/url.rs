//! `new URL(url).href` for the `http` and `https` URLs a buffered request is sent to (`globalFetch` passes
//! `new URL(url)` to `fetch`). Forms whose serialization depends on more of the WHATWG algorithm than this models
//! (credentials, IPv6 and numeric hosts, non-ASCII hosts, dot segments, a lone surrogate) are declined.

use super::BuildError;

const DECLINED: BuildError = BuildError::Unsupported("url-form");

fn encode(byte: u8, set: fn(u8) -> bool, output: &mut String) {
    if set(byte) { output.push_str(&format!("%{byte:02X}")); } else { output.push(byte as char); }
}

fn path_set(byte: u8) -> bool { byte <= 0x20 || byte >= 0x7f || matches!(byte, b'"' | b'<' | b'>' | b'`' | b'{' | b'}' | b'#' | b'?') }
fn query_set(byte: u8) -> bool { byte <= 0x20 || byte >= 0x7f || matches!(byte, b'"' | b'<' | b'>' | b'#' | b'\'') }
fn fragment_set(byte: u8) -> bool { byte <= 0x20 || byte >= 0x7f || matches!(byte, b'"' | b'<' | b'>' | b'`') }

/// A path segment that `new URL` would resolve away.
fn is_dot_segment(segment: &str) -> bool {
    let lower = segment.to_ascii_lowercase().replace("%2e", ".");
    lower == "." || lower == ".."
}

pub(crate) fn href(source: &str) -> Result<String, BuildError> {
    let cleaned: String = source.trim_matches(|c: char| c <= ' ').chars().filter(|c| !matches!(c, '\t' | '\n' | '\r')).collect();
    let (scheme, rest) = cleaned.split_once("://").ok_or(DECLINED)?;
    let scheme = scheme.to_ascii_lowercase();
    let default_port = match scheme.as_str() { "http" => 80, "https" => 443, _ => return Err(DECLINED) };
    let authority_end = rest.find(['/', '?', '#', '\\']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_end);
    if authority.contains('@') || authority.starts_with('[') { return Err(DECLINED); }
    let (host, port) = match authority.rsplit_once(':') { Some((host, port)) => (host, Some(port)), None => (authority, None) };
    if host.is_empty() || !host.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_')) { return Err(DECLINED); }
    let last_label = host.trim_end_matches('.').rsplit('.').next().unwrap_or("");
    if last_label.bytes().all(|byte| byte.is_ascii_digit()) { return Err(DECLINED); }
    let host = host.to_ascii_lowercase();
    let port = match port {
        None | Some("") => None,
        Some(digits) => {
            if !digits.bytes().all(|byte| byte.is_ascii_digit()) { return Err(DECLINED); }
            let value: u32 = digits.trim_start_matches('0').parse().unwrap_or(0);
            if value > 65535 { return Err(DECLINED); }
            (value != default_port).then_some(value)
        }
    };
    let (before_fragment, fragment) = match tail.split_once('#') { Some((head, fragment)) => (head, Some(fragment)), None => (tail, None) };
    let (path, query) = match before_fragment.split_once('?') { Some((path, query)) => (path, Some(query)), None => (before_fragment, None) };
    let path = path.replace('\\', "/");
    let mut output = format!("{scheme}://{host}");
    if let Some(port) = port { output.push_str(&format!(":{port}")); }
    if path.is_empty() { output.push('/'); } else {
        if path.split('/').any(is_dot_segment) { return Err(DECLINED); }
        for byte in path.bytes() { encode(byte, path_set, &mut output); }
    }
    if let Some(query) = query {
        output.push('?');
        for byte in query.bytes() { encode(byte, query_set, &mut output); }
    }
    if let Some(fragment) = fragment {
        output.push('#');
        for byte in fragment.bytes() { encode(byte, fragment_set, &mut output); }
    }
    Ok(output)
}
