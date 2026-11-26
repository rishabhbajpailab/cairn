//! `HttpTransport` on Workers fetch (outbound to OpenRouter only).

use async_trait::async_trait;
use cairn_http::{HttpError, HttpRequest, HttpResponse, HttpTransport};
use worker::send::SendFuture;
use worker::{Fetch, Headers, Method, Request, RequestInit, js_sys};

#[derive(Clone, Copy, Default)]
pub struct WorkerFetch;

#[async_trait]
impl HttpTransport for WorkerFetch {
    async fn send(&self, req: HttpRequest) -> Result<HttpResponse, HttpError> {
        SendFuture::new(async move {
            let net = |e: worker::Error| HttpError::Network(e.to_string());
            let headers = Headers::new();
            for (k, v) in &req.headers {
                headers.set(k, v).map_err(net)?;
            }
            let mut init = RequestInit::new();
            init.with_method(Method::from(req.method.to_string())).with_headers(headers);
            if let Some(b) = &req.body {
                init.with_body(Some(js_sys::Uint8Array::from(b.as_slice()).into()));
            }
            let wreq = Request::new_with_init(&req.url, &init).map_err(net)?;
            let mut resp = Fetch::Request(wreq).send().await.map_err(net)?;
            let status = resp.status_code();
            Ok(HttpResponse { status, body: resp.bytes().await.map_err(net)? })
        })
        .await
    }
}
