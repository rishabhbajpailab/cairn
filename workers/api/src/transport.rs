//! `cairn_http::HttpTransport` on Workers `fetch`. Workers are single-threaded, so wrapping
//! the JS futures in `SendFuture` is sound (see `worker::send`).

use async_trait::async_trait;
use cairn_http::{HttpError, HttpRequest, HttpResponse, HttpTransport};
use worker::send::SendFuture;
use worker::{Fetch, Headers, Method, Request, RequestInit, js_sys};

#[derive(Clone, Copy, Default)]
pub struct WorkerFetch;

pub fn to_worker_request(req: &HttpRequest) -> worker::Result<Request> {
    let headers = Headers::new();
    for (k, v) in &req.headers {
        headers.set(k, v)?;
    }
    let mut init = RequestInit::new();
    init.with_method(Method::from(req.method.to_string())).with_headers(headers);
    if let Some(b) = &req.body {
        init.with_body(Some(js_sys::Uint8Array::from(b.as_slice()).into()));
    }
    Request::new_with_init(&req.url, &init)
}

#[async_trait]
impl HttpTransport for WorkerFetch {
    async fn send(&self, req: HttpRequest) -> Result<HttpResponse, HttpError> {
        SendFuture::new(async move {
            let wreq = to_worker_request(&req).map_err(|e| HttpError::Network(e.to_string()))?;
            let mut resp = Fetch::Request(wreq).send().await.map_err(|e| HttpError::Network(e.to_string()))?;
            let status = resp.status_code();
            let body = resp.bytes().await.map_err(|e| HttpError::Network(e.to_string()))?;
            Ok(HttpResponse { status, body })
        })
        .await
    }
}
