use crate::{Capture, CaptureGuard, Side};
use ::reqwest::{Body, Client, Request, RequestBuilder, Response};
use http_body::{Body as HttpBody, Frame, SizeHint};
use std::{
    pin::Pin,
    task::{Context, Poll},
};

pub trait RequestBuilderExt {
    fn send_logged(
        self,
        source: &'static str,
    ) -> impl std::future::Future<Output = Result<Response, ::reqwest::Error>> + Send;
}
impl RequestBuilderExt for RequestBuilder {
    async fn send_logged(self, source: &'static str) -> Result<Response, ::reqwest::Error> {
        if !crate::enabled() {
            return self.send().await;
        }
        let (client, request) = self.build_split();
        execute(&client, request?, source).await
    }
}
pub async fn execute(
    client: &Client,
    mut request: Request,
    source: &str,
) -> Result<Response, ::reqwest::Error> {
    if !crate::enabled() {
        return client.execute(request).await;
    }
    let capture = crate::begin(
        source,
        request.method().as_str(),
        request.url().as_str(),
        request.headers(),
    );
    if let Some(body) = request.body_mut().take() {
        let body = if let Some(bytes) = body.as_bytes() {
            // Keep reusable bodies reusable, including the client's redirect/retry behavior.
            capture.append(Side::Request, bytes);
            body
        } else {
            Body::wrap(ObservedBody::request(body, capture.0.clone()))
        };
        *request.body_mut() = Some(body);
    }
    match client.execute(request).await {
        Ok(response) => {
            capture.response(
                response.status().as_u16(),
                response.url().as_str(),
                response.headers(),
            );
            Ok(observe_response(response, capture))
        }
        Err(error) => {
            capture.finish("error");
            Err(error)
        }
    }
}
fn observe_response(response: Response, capture: CaptureGuard) -> Response {
    use ::reqwest::ResponseBuilderExt;
    let url = response.url().clone();
    let response: http::Response<Body> = response.into();
    let (mut parts, body) = response.into_parts();
    // reqwest's inverse conversion does not carry its response URL.
    let url_holder = http::Response::builder().url(url).body(()).unwrap();
    parts
        .extensions
        .extend(url_holder.into_parts().0.extensions);
    http::Response::from_parts(parts, Body::wrap(ObservedBody::response(body, capture))).into()
}

pub struct ObservedBody<B: HttpBody + Unpin> {
    inner: B,
    capture: Capture,
    guard: Option<CaptureGuard>,
    side: Side,
}
impl<B: HttpBody + Unpin> ObservedBody<B> {
    pub fn request(inner: B, capture: Capture) -> Self {
        Self {
            inner,
            capture,
            guard: None,
            side: Side::Request,
        }
    }
    pub fn response(inner: B, guard: CaptureGuard) -> Self {
        Self {
            inner,
            capture: guard.0.clone(),
            guard: Some(guard),
            side: Side::Response,
        }
    }
}
impl<B: HttpBody<Data = bytes::Bytes> + Unpin> HttpBody for ObservedBody<B> {
    type Data = B::Data;
    type Error = B::Error;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let result = Pin::new(&mut self.inner).poll_frame(cx);
        match &result {
            Poll::Ready(Some(Ok(frame))) => {
                if let Some(bytes) = frame.data_ref() {
                    self.capture.append(self.side, bytes);
                }
            }
            Poll::Ready(Some(Err(_))) => self.capture.finish("error"),
            _ => {}
        }
        if self.guard.is_some()
            && (matches!(&result, Poll::Ready(None)) || self.inner.is_end_stream())
        {
            self.capture.finish("complete");
        }
        result
    }
    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }
    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}
impl<B: HttpBody + Unpin> Drop for ObservedBody<B> {
    fn drop(&mut self) {
        if self.guard.is_some() && self.inner.is_end_stream() {
            self.capture.finish("complete");
        }
    }
}
