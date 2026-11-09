//! A request ready for transport and the way its URL, headers and body reach `fetch`.

use super::js::{self, J};
use super::BuildError;
use risunest_ir::provider::{HttpMethod, HttpRequestSpec, ResponseKind};
use risunest_ir::{Budget, Text};

/// A request ready for transport, with the state a tool-call follow-up request continues from.
///
/// `spec.url` is what TS hands to `fetch`: the URL as built for a streaming request, and `new URL(url).href` for a
/// buffered one (`globalFetch` constructs the `URL`). `raw_url` is the URL before that, which the preview shows.
/// `Debug` leaves out the body and every header value except `Content-Type`, and cuts the URL at its query.
pub struct BuiltRequest {
    pub spec: HttpRequestSpec,
    pub raw_url: String,
    pub(crate) body: J,
    pub(crate) headers: J,
}

impl std::fmt::Debug for BuiltRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let cut = |url: &str| url.split(['?', '#']).next().unwrap_or("").to_owned();
        let headers: Vec<(&str, &str)> = self.spec.headers.iter()
            .map(|(name, value)| (name.as_str(), if name.eq_ignore_ascii_case("content-type") { value.as_str() } else { "<redacted>" })).collect();
        formatter.debug_struct("BuiltRequest")
            .field("method", &self.spec.method)
            .field("url", &cut(&self.spec.url))
            .field("raw_url", &cut(&self.raw_url))
            .field("headers", &headers)
            .field("body_bytes", &self.spec.body.len())
            .field("response", &self.spec.response)
            .finish()
    }
}

impl BuiltRequest {
    /// The `previewBody` result: `JSON.stringify({ url, body, headers })` with the headers in the order TS built them.
    pub fn preview(&self, budget: &Budget) -> Result<Text, BuildError> {
        let mut object = js::new_object();
        js::set_str(&mut object, "url", J::Str(Text::from_str(&self.raw_url, budget)?), budget)?;
        js::set_str(&mut object, "body", js::clone_j(&self.body, budget)?, budget)?;
        js::set_str(&mut object, "headers", js::clone_j(&self.headers, budget)?, budget)?;
        let bytes = super::wire::stringify(&object, budget)?.ok_or(BuildError::Unsupported("undefined-body"))?;
        Ok(js::text(std::str::from_utf8(&bytes).map_err(|_| BuildError::Unsupported("preview-utf8"))?, budget)?)
    }
}

pub(crate) fn utf8(value: &Text) -> Result<String, BuildError> {
    value.as_unicode().map(str::to_owned).ok_or(BuildError::Unsupported("isolated-surrogate-in-header-or-url"))
}

/// The headers `fetch` receives: `globalFetch` merges `{ 'Content-Type': 'application/json', ...headers }` for a
/// buffered response, so `Content-Type` comes first; the streaming path sends the headers as built.
pub(crate) fn wire_headers(headers: &J, kind: ResponseKind) -> Result<Vec<(String, String)>, BuildError> {
    let J::Obj(entries) = headers else { return Ok(Vec::new()) };
    let mut output: Vec<(String, String)> = Vec::new();
    for (key, value) in entries {
        let J::Str(value) = value else { continue };
        output.push((utf8(key)?, utf8(value)?));
    }
    if kind == ResponseKind::Buffered {
        match output.iter().position(|(name, _)| name == "Content-Type") {
            Some(at) => { let entry = output.remove(at); output.insert(0, entry); }
            None => output.insert(0, ("Content-Type".to_owned(), "application/json".to_owned())),
        }
    }
    Ok(output)
}

pub(crate) fn finish(url: String, body: J, headers_object: &J, stream: bool, budget: &Budget) -> Result<BuiltRequest, BuildError> {
    let kind = if stream { ResponseKind::Sse } else { ResponseKind::Buffered };
    let bytes = super::wire::stringify(&body, budget)?.ok_or(BuildError::Unsupported("undefined-body"))?;
    let headers = wire_headers(headers_object, kind)?;
    let wire_url = if stream { url.clone() } else { super::url::href(&url)? };
    let spec = HttpRequestSpec { method: HttpMethod::Post, url: wire_url, headers, body: bytes, response: kind };
    Ok(BuiltRequest { spec, raw_url: url, body, headers: js::clone_j(headers_object, budget)? })
}

