use crate::model::{Header, METADATA_BYTES};

pub fn headers(values: &http::HeaderMap, hide: bool) -> Result<Vec<Header>, String> {
    let mut total = 0;
    let mut result = Vec::new();
    for (name, value) in values {
        total += name.as_str().len() + value.as_bytes().len() + 64;
        if total > METADATA_BYTES || result.len() >= 512 {
            return Err("metadata-limit".into());
        }
        let masked = hide
            .then(|| mask(name.as_str(), value.as_bytes()))
            .flatten();
        let (value, encoding) = if let Some(value) = masked {
            (value, None)
        } else if let Ok(value) = std::str::from_utf8(value.as_bytes()) {
            (value.to_owned(), None)
        } else {
            (hex(value.as_bytes()), Some("hex".into()))
        };
        result.push(Header {
            name: name.to_string(),
            value,
            encoding,
        });
    }
    Ok(result)
}
pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(DIGITS[(b >> 4) as usize] as char);
        out.push(DIGITS[(b & 15) as usize] as char);
    }
    out
}
fn opaque(value: &str) -> String {
    if ["gho_", "ghp_", "ghs_", "ghu_", "github_pat_", "tid="]
        .iter()
        .any(|p| value.starts_with(p))
        || !value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
        || value.len() < 16
    {
        return "[REDACTED]".into();
    }
    if value.len() < 32 {
        format!("••••••{}", &value[value.len() - 4..])
    } else {
        format!("{}••••••{}", &value[..2], &value[value.len() - 6..])
    }
}
pub fn url(value: &str, hide: bool) -> String {
    let Some((head, rest)) = value.split_once('?') else {
        return value.to_owned();
    };
    if !hide {
        return value.to_owned();
    }
    let (query, fragment) = match rest.split_once('#') {
        Some((query, fragment)) => (query, Some(fragment)),
        None => (rest, None),
    };
    let query = query
        .split('&')
        .map(|pair| match pair.split_once('=') {
            Some((name, key))
                if ["key", "api_key", "apikey"]
                    .iter()
                    .any(|known| name.eq_ignore_ascii_case(known)) =>
            {
                format!("{name}={}", opaque(key))
            }
            _ => pair.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("&");
    match fragment {
        Some(fragment) => format!("{head}?{query}#{fragment}"),
        None => format!("{head}?{query}"),
    }
}
pub fn mask(name: &str, bytes: &[u8]) -> Option<String> {
    let name = name.to_ascii_lowercase();
    match name.as_str() {
        "cookie" | "set-cookie" | "x-amz-security-token" => Some("[REDACTED]".into()),
        "x-api-key" | "api-key" | "x-goog-api-key" => {
            Some(opaque(std::str::from_utf8(bytes).unwrap_or("")))
        }
        "authorization" | "proxy-authorization" => {
            let value = std::str::from_utf8(bytes).unwrap_or("");
            if let Some((scheme, token)) = value.split_once(' ') {
                if scheme.eq_ignore_ascii_case("bearer") || scheme.eq_ignore_ascii_case("token") {
                    return Some(format!("{scheme} {}", opaque(token)));
                }
                if ["basic", "digest", "aws", "aws4-hmac-sha256", "negotiate"]
                    .iter()
                    .any(|s| scheme.eq_ignore_ascii_case(s))
                {
                    return Some(format!("{scheme} [REDACTED]"));
                }
            }
            Some("[REDACTED]".into())
        }
        _ => None,
    }
}
