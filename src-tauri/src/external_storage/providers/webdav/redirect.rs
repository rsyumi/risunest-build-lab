use super::*;

pub(super) enum Body<'a> {
    Empty,
    Bytes(&'a [u8]),
    Source(&'a dyn TransferSource),
}

impl WebdavProvider {
    pub(super) async fn send_redirects(
        &self,
        mut request: HttpRequest,
        body: Body<'_>,
        cancel: &Cancellation,
    ) -> Result<(HttpResponse, Url)> {
        let mut visited = Vec::new();
        loop {
            cancel.check()?;
            if visited.contains(&request.url) {
                return Err(ProviderError::new(ErrorKind::EndpointRejected).caused("WebDAV redirect loop detected"));
            }
            visited.push(request.url.clone());
            let (reader, length): (Option<std::pin::Pin<Box<dyn tokio::io::AsyncRead + Send>>>, _) = match &body {
                Body::Empty => (None, None),
                Body::Bytes(bytes) => (Some(Box::pin(std::io::Cursor::new(bytes.to_vec()))), Some(bytes.len() as u64)),
                Body::Source(source) => (Some(source.open(0, source.byte_length(), cancel).await?), Some(source.byte_length())),
            };
            let response = self.dependencies.send(HttpRequest {
                method: request.method.clone(), url: request.url.clone(), headers: request.headers.clone(),
                body: reader, content_length: length, operation: request.operation, account: request.account.clone(),
                api_request: request.api_request, mybox_charge: None, control: request.control,
            }, cancel).await?;
            if !matches!(response.status, 301 | 302 | 303 | 307 | 308) {
                return Ok((response, request.url));
            }
            let rejection = |detail: &str| common::error(ErrorKind::EndpointRejected, response.status).caused(detail);
            // A GET of a 303 result page cannot confirm a DAV write or supply DAV metadata.
            if response.status == 303 && !matches!(request.method, reqwest::Method::GET | reqwest::Method::HEAD) {
                return Err(rejection(&format!("WebDAV returned HTTP 303 for {}; a result-page GET cannot confirm this operation. Use a direct WebDAV URL or a method-preserving redirect", request.method)));
            }
            if visited.len() > 10 {
                return Err(rejection("WebDAV exceeded 10 redirects"));
            }
            let location = response.headers.get("location")
                .ok_or_else(|| rejection("WebDAV redirect response has no Location header"))?;
            let mut next = request.url.join(location)
                .map_err(|error| rejection("WebDAV redirect Location is not a valid URL").caused(&error))?;
            next.set_fragment(None);
            if !http::user_endpoint_allowed(&next) {
                return Err(rejection("WebDAV redirects must use HTTP or HTTPS without embedded credentials"));
            }
            if request.url.origin() != next.origin() {
                request.headers.retain(|key, _| !matches!(key.to_ascii_lowercase().as_str(),
                    "authorization" | "proxy-authorization" | "cookie" | "host"));
                request.api_request = false;
            }
            request.url = next;
        }
    }
}
