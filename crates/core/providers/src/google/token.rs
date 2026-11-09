//! The Vertex service-account access token (`google.ts:418-507`): the RS256 JWT TS signs with `crypto.subtle`, the form
//! request that trades it for an access token, and the reading of that reply. The signing uses `ring`; the clock is an
//! input and the cache the owner keeps (`db.vertexAccessToken`, `db.vertexAccessTokenExpires`) is an explicit output.

use crate::common::js::{self, J};
use crate::common::owner::{error_message, json_parse_error, response_text};
use crate::common::wire;
use crate::common::BuildError;
use risunest_ir::provider::{HttpMethod, HttpRequestSpec, ResponseKind};
use risunest_ir::{json, Budget, Error, Text, Value};

const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const BEGIN: &str = "-----BEGIN PRIVATE KEY-----";
const END: &str = "-----END PRIVATE KEY-----";
/// `db2.vertexAccessTokenExpires = Date.now() + 3500 * 1000`.
pub const TOKEN_LIFETIME_MS: i64 = 3_500_000;

/// What the request builder does about the `Authorization` header of a Vertex request.
#[derive(Debug, PartialEq, Eq)]
pub enum TokenPlan {
    /// `db.vertexAccessTokenExpires >= Date.now()`: `Bearer <db.vertexAccessToken>`.
    Cached,
    /// The token expired and the client email or private key is empty: TS alerts and returns `{ type: 'fail' }`.
    Missing,
    /// The token expired: mint a new one with [`token_request`].
    Mint,
}

/// `db.vertexAccessTokenExpires < Date.now()` and the credentials checks that follow it. A cached token that has not
/// expired is used as it is, whether or not the credentials are filled in.
pub fn plan(expires_ms: f64, now_ms: i64, client_email: &Text, private_key: &Text) -> TokenPlan {
    // `undefined < n` is false, so a missing expiry keeps the cached token.
    if expires_ms.is_nan() || expires_ms >= now_ms as f64 { return TokenPlan::Cached; }
    if client_email.len_units() == 0 || private_key.len_units() == 0 { TokenPlan::Missing } else { TokenPlan::Mint }
}

/// The failure text of [`TokenPlan::Missing`].
pub const MISSING_CREDENTIALS: &str = "Vertex AI authentication information is missing or incomplete. Please check your settings.";

fn contains(text: &Text, needle: &str) -> bool { js::find_units(&js::units(text), &js::utf16(needle), 0).is_some() }

fn thrown(message: &str, budget: &Budget) -> BuildError {
    match js::text(message, budget) { Ok(text) => BuildError::Thrown(text), Err(error) => BuildError::Budget(error) }
}

/// `str2ab`: the markers and the two characters `\` `n` are removed, then `atob` reads what is left. `atob` is the
/// forgiving base64 decoder of the HTML standard; its failure is a runtime-specific `DOMException`, so it is declined.
fn der_of(key: &Text) -> Result<Vec<u8>, BuildError> {
    let mut units = js::units(key);
    for pattern in [BEGIN, END, "\\n"] {
        let needle = js::utf16(pattern);
        while let Some(at) = js::find_units(&units, &needle, 0) { units.drain(at..at + needle.len()); }
    }
    // The regular expression removes the alternatives left to right at every position, which is what a pass per
    // pattern does unless removing one joins the halves of another; the markers and `\n` share no characters.
    let mut data: Vec<u8> = Vec::with_capacity(units.len());
    for unit in units {
        if unit > 127 { return Err(BuildError::Unsupported("vertex-key-base64")); }
        if matches!(unit, 9 | 10 | 12 | 13 | 32) { continue; }
        data.push(unit as u8);
    }
    if data.len().is_multiple_of(4) {
        for _ in 0..2 { if data.last() == Some(&b'=') { data.pop(); } }
    }
    if data.len() % 4 == 1 { return Err(BuildError::Unsupported("vertex-key-base64")); }
    let mut bytes = Vec::with_capacity(data.len() * 3 / 4);
    let (mut accumulator, mut bits) = (0u32, 0u32);
    for byte in data {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return Err(BuildError::Unsupported("vertex-key-base64")),
        };
        accumulator = (accumulator << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 { bits -= 8; bytes.push((accumulator >> bits) as u8); accumulator &= (1 << bits) - 1; }
    }
    Ok(bytes)
}

/// `base64url` of `util.ts`: the standard alphabet with `-` and `_`, without padding.
pub(crate) fn base64url(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut text = String::with_capacity(bytes.len() * 4 / 3 + 3);
    for chunk in bytes.chunks(3) {
        let value = chunk.iter().enumerate().fold(0u32, |all, (at, byte)| all | (u32::from(*byte) << (16 - 8 * at)));
        for at in 0..=chunk.len() { text.push(ALPHABET[((value >> (18 - 6 * at)) & 63) as usize] as char); }
    }
    text
}

fn object_text(entries: Vec<(&str, J)>, budget: &Budget) -> Result<Vec<u8>, BuildError> {
    let mut object = js::new_object();
    for (key, value) in entries { js::set_str(&mut object, key, value, budget)?; }
    wire::stringify(&object, budget)?.ok_or(BuildError::Unsupported("undefined-body"))
}

/// `generateToken` up to the `fetch`: validates the credentials, signs the claim set at `now_ms` and returns the request
/// that trades the JWT for an access token. This request is sent with plain `fetch`, not `fetchNative`, and carries no
/// abort signal.
pub fn token_request(client_email: &Text, private_key: &Text, now_ms: i64, budget: &Budget) -> Result<HttpRequestSpec, BuildError> {
    if !contains(client_email, "gserviceaccount.com") { return Err(thrown("Invalid Vertex client email. Must include gserviceaccount.com", budget)); }
    if !contains(private_key, BEGIN) || !contains(private_key, END) { return Err(thrown("Invalid Vertex private key. Must include proper key markers.", budget)); }
    let der = der_of(private_key)?;
    let time = now_ms.div_euclid(1000);
    let header = object_text(vec![("alg", js::str_value("RS256", budget)?), ("typ", js::str_value("JWT", budget)?)], budget)?;
    let claims = object_text(vec![
        ("iss", J::Str(client_email.copy(budget)?)),
        ("iat", J::Num(time as f64)),
        ("exp", J::Num((time + 3600) as f64)),
        ("scope", js::str_value("https://www.googleapis.com/auth/cloud-platform", budget)?),
        ("aud", js::str_value(TOKEN_URL, budget)?),
    ], budget)?;
    let signing_input = format!("{}.{}", base64url(&header), base64url(&claims));
    // `crypto.subtle.importKey` rejects what it cannot read with a runtime-specific error; `ring` is stricter than some
    // runtimes about the key form, so either way the request is declined rather than guessed.
    let pair = ring::signature::RsaKeyPair::from_pkcs8(&der).map_err(|_| BuildError::Unsupported("vertex-key-import"))?;
    let mut signature = vec![0u8; pair.public().modulus_len()];
    pair.sign(&ring::signature::RSA_PKCS1_SHA256, &ring::rand::SystemRandom::new(), signing_input.as_bytes(), &mut signature)
        .map_err(|_| BuildError::Unsupported("vertex-key-sign"))?;
    let jwt = format!("{signing_input}.{}", base64url(&signature));
    Ok(HttpRequestSpec {
        method: HttpMethod::Post,
        url: TOKEN_URL.to_owned(),
        headers: vec![("Content-Type".to_owned(), "application/x-www-form-urlencoded".to_owned())],
        body: format!("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer&assertion={jwt}").into_bytes(),
        response: ResponseKind::Buffered,
    })
}

/// A minted token, which the owner stores as `db.vertexAccessToken` and `db.vertexAccessTokenExpires`.
#[derive(Debug)]
pub struct MintedToken {
    pub token: Text,
    pub expires_ms: i64,
}

/// The reply to [`token_request`]. `now_ms` is `Date.now()` once the reply has been read. Every failure is a TS throw
/// ([`BuildError::Thrown`]); a transport error is passed as its `String(error)`.
pub fn read_token(reply: Result<(u16, &[u8]), &Text>, now_ms: i64, budget: &Budget) -> Result<MintedToken, BuildError> {
    let (status, body) = match reply {
        Ok(found) => found,
        Err(error) => return Err(BuildError::Thrown(error_message(error, budget)?)),
    };
    let text = response_text(body, budget)?;
    let source = String::from_utf16_lossy(&js::units(&text));
    let parsed = json::parse(&source, budget);
    if !(200..300).contains(&status) {
        let shown = match parsed {
            Ok(value) => match json::stringify(&value, budget)? { Some(text) => text, None => js::text(&status.to_string(), budget)? },
            Err(Error::Syntax { .. }) => js::text(&status.to_string(), budget)?,
            Err(other) => return Err(other.into()),
        };
        let mut message = js::text("Failed to refresh google access token: ", budget)?;
        message.append(&shown)?;
        return Err(BuildError::Thrown(message));
    }
    let value = match parsed {
        Ok(value) => value,
        Err(Error::Syntax { .. }) => return Err(BuildError::Thrown(error_message(&json_parse_error(&text, budget)?, budget)?)),
        Err(other) => return Err(other.into()),
    };
    let token = match &value {
        Value::Null => return Err(thrown("Cannot read properties of null (reading 'access_token')", budget)),
        Value::Object(entries) => entries.as_slice().iter().find(|(name, _)| name.equals_ascii(b"access_token")).map(|(_, value)| value),
        _ => None,
    };
    match token {
        None | Some(Value::Undefined | Value::Null) => Err(thrown("No google access token in the response", budget)),
        Some(Value::Text(token)) if token.len_units() == 0 => Err(thrown("No google access token in the response", budget)),
        Some(Value::Text(token)) => Ok(MintedToken { token: token.copy(budget)?, expires_ms: now_ms + TOKEN_LIFETIME_MS }),
        Some(Value::Bool(false)) => Err(thrown("No google access token in the response", budget)),
        Some(Value::Number(number)) if *number == 0.0 || number.is_nan() => Err(thrown("No google access token in the response", budget)),
        Some(_) => Err(BuildError::Unsupported("vertex-token-not-text")),
    }
}
