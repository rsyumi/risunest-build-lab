//! The Bedrock branch of `requestClaude` (`anthropic.ts:400-512`): the model route, the request body and the AWS
//! Signature Version 4 that `@smithy/signature-v4` computes for it. The clock is an input so a fixed time gives fixed
//! bytes.

use super::BuildError;
use crate::common::built::{finish, utf8, BuiltRequest};
use crate::common::js::{self, J};
use crate::common::params;
use crate::common::sha256::{hex, hmac_sha256, sha256};
use crate::common::wire;
use risunest_ir::{Budget, Text};

const SERVICE: &str = "bedrock";
const ANTHROPIC_VERSION: &str = "bedrock-2023-05-31";
const PAYLOAD_HEADER: &str = "x-amz-content-sha256";

/// Headers `@smithy/signature-v4` never signs (`ALWAYS_UNSIGNABLE_HEADERS`). The object it keeps them in is read with
/// `in`, which also finds the properties `Object.prototype` has under these lowercase names.
const ALWAYS_UNSIGNABLE: [&str; 17] = [
    "authorization", "cache-control", "connection", "expect", "from", "keep-alive", "max-forwards", "pragma", "referer", "te", "trailer",
    "transfer-encoding", "upgrade", "user-agent", "x-amzn-trace-id", "constructor", "__proto__",
];

fn thrown(message: &str, budget: &Budget) -> BuildError {
    match js::text(message, budget) {
        Ok(text) => BuildError::Thrown(text),
        Err(error) => BuildError::Budget(error),
    }
}

fn is_digit(unit: u8) -> bool { unit.is_ascii_digit() }

/// `Number(id.match(/(\d{8})/)?.[0])`: the first run of eight digits, or `None` when there is none.
fn date_part(id: &[u8]) -> Option<f64> {
    (0..id.len().saturating_sub(7)).find(|&start| id[start..start + 8].iter().copied().all(is_digit))
        .map(|start| std::str::from_utf8(&id[start..start + 8]).expect("digits").parse::<f64>().expect("digits"))
}

/// `id.match(/claude-(?:opus-|sonnet-|haiku-)?(\d+)-(\d+)/)`: the major and minor numbers of the leftmost match.
fn version_part(id: &[u8]) -> Option<(f64, f64)> {
    let digits_at = |from: usize| -> usize { id[from..].iter().take_while(|unit| is_digit(**unit)).count() };
    for start in 0..id.len() {
        let Some(rest) = id[start..].strip_prefix(b"claude-") else { continue };
        let offset = start + id.len() - start - rest.len();
        let mut tries = vec![offset];
        for prefix in [&b"opus-"[..], b"sonnet-", b"haiku-"] {
            if id[offset..].starts_with(prefix) { tries.insert(0, offset + prefix.len()); break; }
        }
        for from in tries {
            let major = digits_at(from);
            if major == 0 || id.get(from + major) != Some(&b'-') { continue; }
            let minor_from = from + major + 1;
            let minor = digits_at(minor_from);
            if minor == 0 { continue; }
            let number = |range: std::ops::Range<usize>| std::str::from_utf8(&id[range]).expect("digits").parse::<f64>().expect("digits");
            return Some((number(from..from + major), number(minor_from..minor_from + minor)));
        }
    }
    None
}

/// Whether the model id selects the `global.` inference profile instead of `us.`.
fn uses_global_profile(id: &[u8]) -> bool {
    match date_part(id) {
        Some(date) if date != 0.0 => date >= 20250929.0,
        _ => match version_part(id) {
            Some((major, minor)) => major > 4.0 || (major == 4.0 && minor >= 5.0),
            None => false,
        },
    }
}

/// `escapeUri`: `encodeURIComponent` that also escapes `!'()*`. The path holds ASCII only.
fn escape_uri(segment: &str) -> String {
    let mut escaped = String::new();
    for byte in segment.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') { escaped.push(byte as char); } else { escaped.push_str(&format!("%{byte:02X}")); }
    }
    escaped
}

/// `new Date(ms).toISOString()` as `SignatureV4` shortens it: `20260102T030405Z` and `20260102`.
fn amz_dates(now_ms: i64) -> Result<(String, String), BuildError> {
    let seconds = now_ms.div_euclid(1000);
    let (days, rest) = (seconds.div_euclid(86_400), seconds.rem_euclid(86_400));
    // Civil date from days since the epoch (proleptic Gregorian).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 { shifted_month + 3 } else { shifted_month - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    if !(0..=9999).contains(&year) { return Err(BuildError::Unsupported("bedrock-clock")); }
    let short = format!("{year:04}{month:02}{day:02}");
    let long = format!("{short}T{:02}{:02}{:02}Z", rest / 3600, rest % 3600 / 60, rest % 60);
    Ok((long, short))
}

/// The canonical form of a header value: line breaks and runs of spaces and tabs become one space, and one space is
/// trimmed from each end.
fn canonical_value(value: &str) -> String {
    let mut collapsed = String::new();
    let mut in_run = false;
    for character in value.chars() {
        if matches!(character, ' ' | '\t' | '\r' | '\n') {
            if !in_run { collapsed.push(' '); }
            in_run = true;
        } else {
            collapsed.push(character);
            in_run = false;
        }
    }
    let trimmed = collapsed.strip_prefix(' ').unwrap_or(&collapsed);
    trimmed.strip_suffix(' ').unwrap_or(trimmed).to_owned()
}

struct Credentials<'a> { access_key_id: &'a str, secret_access_key: &'a str, region: &'a str }

/// `SignatureV4.sign` for a `POST` without a query: the request headers gain `x-amz-date`, `x-amz-content-sha256`
/// (unless the request has one) and `authorization`; `authorization`, `x-amz-date` and `date` are replaced.
fn sign(headers: &mut Vec<(String, String)>, path: &str, body: &[u8], credentials: &Credentials<'_>, now_ms: i64) -> Result<(), BuildError> {
    let (long_date, short_date) = amz_dates(now_ms)?;
    headers.retain(|(name, _)| !matches!(name.to_ascii_lowercase().as_str(), "authorization" | "x-amz-date" | "date"));
    headers.push(("x-amz-date".to_owned(), long_date.clone()));
    let present = headers.iter().find(|(name, _)| name.eq_ignore_ascii_case(PAYLOAD_HEADER)).map(|(_, value)| value.clone());
    let payload_hash = match present {
        Some(value) => value,
        None => {
            let computed = hex(&sha256(body));
            headers.push((PAYLOAD_HEADER.to_owned(), computed.clone()));
            computed
        }
    };

    let mut sorted: Vec<&(String, String)> = headers.iter().collect();
    sorted.sort_by(|left, right| left.0.cmp(&right.0));
    let mut canonical: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for (name, value) in sorted {
        let lower = name.to_ascii_lowercase();
        if ALWAYS_UNSIGNABLE.contains(&lower.as_str()) || lower.starts_with("proxy-") || lower.starts_with("sec-") { continue; }
        canonical.insert(lower, canonical_value(value));
    }
    let signed_headers = canonical.keys().cloned().collect::<Vec<_>>().join(";");
    let header_lines = canonical.iter().map(|(name, value)| format!("{name}:{value}")).collect::<Vec<_>>().join("\n");
    let canonical_path = path.split('/').map(escape_uri).collect::<Vec<_>>().join("/");
    let canonical_request = format!("POST\n{canonical_path}\n\n{header_lines}\n\n{signed_headers}\n{payload_hash}");

    let scope = format!("{short_date}/{}/{SERVICE}/aws4_request", credentials.region);
    let string_to_sign = format!("AWS4-HMAC-SHA256\n{long_date}\n{scope}\n{}", hex(&sha256(canonical_request.as_bytes())));
    let mut key = hmac_sha256(format!("AWS4{}", credentials.secret_access_key).as_bytes(), short_date.as_bytes()).to_vec();
    for part in [credentials.region, SERVICE, "aws4_request"] { key = hmac_sha256(&key, part.as_bytes()).to_vec(); }
    let signature = hex(&hmac_sha256(&key, string_to_sign.as_bytes()));
    headers.push((
        "authorization".to_owned(),
        format!("AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={signature}", credentials.access_key_id),
    ));
    Ok(())
}

/// Builds the signed request. `params` is the body `requestClaude` has assembled, `api_key` is `id:secret:region`, and
/// `now_ms` is the clock (`Date.now()`) the signature is made at.
pub(crate) fn build(
    mut params: J, api_key: &Text, internal_id: Option<&Text>, additional: &[(Text, Text)], now_ms: i64, budget: &Budget,
) -> Result<BuiltRequest, BuildError> {
    let units = js::units(api_key);
    let mut parts = units.split(|unit| *unit == u16::from(b':'));
    let mut next = || -> Option<&[u16]> { parts.next().filter(|part| !part.is_empty()) };
    let (Some(id), Some(secret), Some(region)) = (next(), next(), next()) else {
        return Err(thrown("The key assigned to this request is invalid.", budget));
    };
    let text_of = |part: &[u16]| js::from_units(part, budget).map_err(BuildError::from).and_then(|text| utf8(&text));
    let (access_key_id, secret_access_key, region) = (text_of(id)?, text_of(secret)?, text_of(region)?);
    if !region.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'-') { return Err(BuildError::Unsupported("bedrock-region-form")); }

    let Some(internal_id) = internal_id else { return Err(thrown("Cannot read properties of undefined (reading 'match')", budget)) };
    let model = utf8(internal_id)?;
    if !model.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-')) {
        return Err(BuildError::Unsupported("bedrock-model-form"));
    }
    let profile = if uses_global_profile(model.as_bytes()) { "global." } else { "us." };
    let host = format!("bedrock-runtime.{region}.amazonaws.com");
    let path = format!("/model/{profile}{model}/invoke");
    let url = format!("https://{host}{path}");

    js::set_str(&mut params, "anthropic_version", js::str_value(ANTHROPIC_VERSION, budget)?, budget)?;
    js::delete(&mut params, "model");
    js::delete(&mut params, "stream");
    let thinking_on = match js::get(&params, "thinking").and_then(|thinking| js::get(thinking, "type")) {
        Some(J::Str(kind)) => kind.equals_ascii(b"enabled") || kind.equals_ascii(b"adaptive"),
        _ => false,
    };
    if thinking_on {
        js::set_str(&mut params, "temperature", J::Num(1.0), budget)?;
        js::delete(&mut params, "top_k");
        js::delete(&mut params, "top_p");
    }

    let mut headers = js::new_object();
    js::set_str(&mut headers, "Host", js::str_value(&host, budget)?, budget)?;
    js::set_str(&mut headers, "Content-Type", js::str_value("application/json", budget)?, budget)?;
    js::set_str(&mut headers, "accept", js::str_value("application/json", budget)?, budget)?;
    if !additional.is_empty() { params::apply_additional_parameters(&mut params, &mut headers, additional, budget)?; }

    let body = wire::stringify(&params, budget)?.ok_or(BuildError::Unsupported("undefined-body"))?;
    let J::Obj(entries) = &headers else { return Err(BuildError::Unsupported("headers")) };
    let mut flat: Vec<(String, String)> = Vec::new();
    for (name, value) in entries {
        let J::Str(value) = value else { return Err(BuildError::Unsupported("header-value-not-text")) };
        let name = utf8(name)?;
        if !name.is_ascii() { return Err(BuildError::Unsupported("header-name-non-ascii")); }
        flat.push((name, utf8(value)?));
    }
    sign(&mut flat, &path, &body, &Credentials { access_key_id: &access_key_id, secret_access_key: &secret_access_key, region: &region }, now_ms)?;

    let mut signed = js::new_object();
    for (name, value) in &flat { js::set_str(&mut signed, name, js::str_value(value, budget)?, budget)?; }
    finish(url, params, &signed, false, budget)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_follow_the_gregorian_calendar() {
        assert_eq!(amz_dates(1_767_323_045_000).unwrap(), ("20260102T030405Z".to_owned(), "20260102".to_owned()));
        assert_eq!(amz_dates(1_835_395_200_999).unwrap().1, "20280229");
        assert_eq!(amz_dates(0).unwrap().0, "19700101T000000Z");
        assert_eq!(amz_dates(-1).unwrap().0, "19691231T235959Z");
        assert!(amz_dates(300_000_000_000_000).is_err());
    }

    #[test]
    fn the_profile_follows_the_date_and_then_the_version() {
        for (id, global) in [
            ("anthropic.claude-sonnet-4-5-20250929-v1:0", true), ("anthropic.claude-sonnet-4-20250514-v1:0", false), ("claude-opus-4-5-v1:0", true),
            ("claude-opus-4-1-v1:0", false), ("claude-5-0-v1", true), ("claude-3-7-sonnet-v1", false), ("claude-haiku-4-5", true),
            ("claude-opus-4-5-00000000", true), ("titan-v1", false), ("m-123456789012-v1", false), ("claude-4-9", true), ("claude-4", false),
        ] {
            assert_eq!(uses_global_profile(id.as_bytes()), global, "{id}");
        }
    }

    #[test]
    fn values_are_canonicalised_like_smithy() {
        assert_eq!(canonical_value("  a   b "), "a b");
        assert_eq!(canonical_value("\ta\t\tb\t"), "a b");
        assert_eq!(canonical_value("one\ntwo\r\nthree"), "one two three");
        assert_eq!(canonical_value("   "), "");
        assert_eq!(canonical_value("\u{a0}a\u{a0}"), "\u{a0}a\u{a0}");
    }
}
