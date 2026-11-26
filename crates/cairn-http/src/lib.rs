//! A deliberately tiny HTTP abstraction.
//!
//! Libraries (`cairn-store`, `cairn-app`, `cairn-gateway-core`) talk HTTP only through
//! [`HttpTransport`]. In production the Workers glue implements it with `worker::Fetch`; tests
//! use [`FakeTransport`]. This keeps every library compiling natively for fast unit tests and
//! to `wasm32-unknown-unknown` for Workers.

use async_trait::async_trait;
use serde::Serialize;
use std::sync::Mutex;

#[derive(Debug, Clone, PartialEq)]
pub struct HttpRequest {
    pub method: &'static str,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

impl HttpRequest {
    pub fn get(url: impl Into<String>) -> Self {
        Self { method: "GET", url: url.into(), headers: vec![], body: None }
    }

    pub fn post_json(url: impl Into<String>, body: &impl Serialize) -> Result<Self, HttpError> {
        let bytes = serde_json::to_vec(body).map_err(|e| HttpError::Encode(e.to_string()))?;
        Ok(Self {
            method: "POST",
            url: url.into(),
            headers: vec![("content-type".into(), "application/json".into())],
            body: Some(bytes),
        })
    }

    pub fn header(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.headers.push((k.into(), v.into()));
        self
    }

    pub fn bearer(self, token: &str) -> Self {
        self.header("authorization", format!("Bearer {token}"))
    }

    pub fn body_json(&self) -> Option<serde_json::Value> {
        self.body.as_ref().and_then(|b| serde_json::from_slice(b).ok())
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

impl HttpResponse {
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    pub fn json(&self) -> Result<serde_json::Value, HttpError> {
        serde_json::from_slice(&self.body).map_err(|e| HttpError::Decode(e.to_string()))
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum HttpError {
    #[error("network: {0}")]
    Network(String),
    #[error("encode: {0}")]
    Encode(String),
    #[error("decode: {0}")]
    Decode(String),
}

#[async_trait]
pub trait HttpTransport: Send + Sync {
    async fn send(&self, req: HttpRequest) -> Result<HttpResponse, HttpError>;
}

/// Test double: records requests and replays canned responses in order.
#[derive(Default)]
pub struct FakeTransport {
    pub requests: Mutex<Vec<HttpRequest>>,
    responses: Mutex<Vec<HttpResponse>>,
}

impl FakeTransport {
    pub fn with_responses(responses: Vec<HttpResponse>) -> Self {
        let mut r = responses;
        r.reverse();
        Self { requests: Mutex::new(vec![]), responses: Mutex::new(r) }
    }

    pub fn json(status: u16, v: serde_json::Value) -> HttpResponse {
        HttpResponse { status, body: v.to_string().into_bytes() }
    }

    pub fn sent(&self) -> Vec<HttpRequest> {
        self.requests.lock().map(|r| r.clone()).unwrap_or_default()
    }
}

#[async_trait]
impl HttpTransport for FakeTransport {
    async fn send(&self, req: HttpRequest) -> Result<HttpResponse, HttpError> {
        if let Ok(mut r) = self.requests.lock() {
            r.push(req);
        }
        self.responses
            .lock()
            .ok()
            .and_then(|mut r| r.pop())
            .ok_or_else(|| HttpError::Network("FakeTransport: no canned response left".into()))
    }
}
