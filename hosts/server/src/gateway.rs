//! The app's `Gateway` port over the private compose network (the old Service Binding).
//! Same paths and bodies as before; `cairn-gateway` has no published port.

use async_trait::async_trait;
use cairn_app::ports::{CompleteRequest, CompleteResponse, Gateway, GatewayError};
use cairn_http::{HttpRequest, HttpTransport};
use serde_json::Value;
use std::sync::Arc;

pub struct GatewayHttp {
    pub http: Arc<dyn HttpTransport>,
    /// e.g. `http://cairn-gateway:8788`
    pub base: String,
}

impl GatewayHttp {
    async fn call(&self, path: &str, body: Option<&Value>) -> Result<Value, GatewayError> {
        let url = format!("{}{path}", self.base.trim_end_matches('/'));
        let req = match body {
            Some(b) => HttpRequest::post_json(url, b).map_err(|e| GatewayError::Unreachable(e.to_string()))?,
            None => HttpRequest::get(url),
        };
        let resp = self.http.send(req).await.map_err(|e| GatewayError::Unreachable(e.to_string()))?;
        let v: Value = resp.json().unwrap_or(Value::Null);
        if resp.is_success() { Ok(v) } else { Err(GatewayError::Refused { status: resp.status, body: v }) }
    }
}

#[async_trait]
impl Gateway for GatewayHttp {
    async fn complete(&self, req: &CompleteRequest) -> Result<CompleteResponse, GatewayError> {
        let body = serde_json::to_value(req).map_err(|e| GatewayError::Unreachable(e.to_string()))?;
        let v = self.call("/v1/complete", Some(&body)).await?;
        serde_json::from_value(v.clone()).map_err(|_| GatewayError::Refused { status: 200, body: v })
    }

    async fn disable(&self) -> Result<(), GatewayError> {
        self.call("/v1/admin/disable", Some(&serde_json::json!({}))).await.map(|_| ())
    }

    async fn status(&self) -> Result<Value, GatewayError> {
        self.call("/v1/status", None).await
    }
}
