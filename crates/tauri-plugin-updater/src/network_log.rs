use reqwest::{Body, RequestBuilder, Response, ResponseBuilderExt};
use risunest_network_log::{reqwest::ObservedBody, Side};

pub(crate) trait RequestBuilderExt {
    fn send_logged(self) -> impl std::future::Future<Output = Result<Response, reqwest::Error>> + Send;
}
impl RequestBuilderExt for RequestBuilder {
    async fn send_logged(self) -> Result<Response, reqwest::Error> {
        if !risunest_network_log::enabled() { return self.send().await; }
        let (client, request) = self.build_split();
        let request = request?;
        let capture = risunest_network_log::begin("updater", request.method().as_str(), request.url().as_str(), request.headers());
        if let Some(bytes) = request.body().and_then(Body::as_bytes) { capture.append(Side::Request, bytes); }
        let response = match client.execute(request).await {
            Ok(response) => response,
            Err(error) => { capture.finish("error"); return Err(error); }
        };
        capture.response(response.status().as_u16(), response.url().as_str(), response.headers());
        let url = response.url().clone();
        let response: http::Response<Body> = response.into();
        let (mut parts, body) = response.into_parts();
        let url_holder = http::Response::builder().url(url).body(()).unwrap();
        parts.extensions.extend(url_holder.into_parts().0.extensions);
        Ok(http::Response::from_parts(parts, Body::wrap(ObservedBody::response(body, capture))).into())
    }
}
