//! `cairn_app::ports` implemented with Cloudflare bindings.

use async_trait::async_trait;
use cairn_app::ports::{
    CaptureMsg, ChangeEvent, CompleteRequest, CompleteResponse, Gateway, GatewayError, Hub, PortError, SchedulerPort,
    Spool,
};
use serde_json::Value;
use worker::send::{SendFuture, SendWrapper};
use worker::{Fetcher, ObjectNamespace, Queue, Request, RequestInit, Response};

/// CAPTURES queue producer.
pub struct QueueSpool(pub SendWrapper<Queue>);

#[async_trait]
impl Spool for QueueSpool {
    async fn enqueue(&self, msg: &CaptureMsg) -> Result<(), PortError> {
        let msg = msg.clone();
        SendFuture::new(async { self.0.send(msg).await }).await.map_err(|e| PortError(e.to_string()))
    }
}

async fn json_post(
    stub_fetch: impl std::future::Future<Output = worker::Result<Response>>,
) -> worker::Result<Response> {
    stub_fetch.await
}

fn post_json(url: &str, body: &impl serde::Serialize) -> worker::Result<Request> {
    let headers = worker::Headers::new();
    headers.set("content-type", "application/json")?;
    let mut init = RequestInit::new();
    init.with_method(worker::Method::Post)
        .with_headers(headers)
        .with_body(Some(serde_json::to_string(body).map_err(|e| worker::Error::RustError(e.to_string()))?.into()));
    Request::new_with_init(url, &init)
}

/// Single, well-known Durable Object instance by name.
fn singleton(ns: &ObjectNamespace, name: &str) -> worker::Result<worker::Stub> {
    ns.id_from_name(name)?.get_stub()
}

/// Hub Durable Object: fan-out of change events to open browser WebSockets.
pub struct HubPort(pub SendWrapper<ObjectNamespace>);

#[async_trait]
impl Hub for HubPort {
    async fn publish(&self, ev: &ChangeEvent) {
        let ev = ev.clone();
        let r = SendFuture::new(async {
            let stub = singleton(&self.0, "hub")?;
            json_post(stub.fetch_with_request(post_json("https://hub/publish", &ev)?)).await
        })
        .await;
        if let Err(e) = r {
            tracing::warn!(error = %e, "hub publish failed (live update skipped)");
        }
    }
}

/// Scheduler Durable Object: recompute the next alarm.
pub struct SchedulerStub(pub SendWrapper<ObjectNamespace>);

#[async_trait]
impl SchedulerPort for SchedulerStub {
    async fn rearm(&self) {
        let r = SendFuture::new(async {
            let stub = singleton(&self.0, "scheduler")?;
            stub.fetch_with_request(post_json("https://scheduler/rearm", &serde_json::json!({}))?).await
        })
        .await;
        if let Err(e) = r {
            tracing::warn!(error = %e, "scheduler rearm failed; hourly cron backstop will catch up");
        }
    }
}

/// Used inside the Scheduler DO itself, so firing reminders doesn't re-enter it.
pub struct NoopScheduler;

#[async_trait]
impl SchedulerPort for NoopScheduler {
    async fn rearm(&self) {}
}

/// Service Binding to the gateway Worker. No public URL exists for the gateway; the host
/// part of the URL below is ignored by Service Bindings.
pub struct GatewayBinding(pub SendWrapper<Fetcher>);

impl GatewayBinding {
    async fn call(&self, path: &str, body: Option<&Value>) -> Result<Value, GatewayError> {
        let body = body.cloned();
        SendFuture::new(async {
            let req = match &body {
                Some(b) => post_json(&format!("https://gateway{path}"), b),
                None => Request::new(&format!("https://gateway{path}"), worker::Method::Get),
            }
            .map_err(|e| GatewayError::Unreachable(e.to_string()))?;
            let resp = self.0.fetch_request(req).await.map_err(|e| GatewayError::Unreachable(e.to_string()))?;
            let mut resp: Response =
                resp.try_into().map_err(|e: worker::Error| GatewayError::Unreachable(e.to_string()))?;
            let status = resp.status_code();
            let v: Value = resp.json().await.unwrap_or(Value::Null);
            if (200..300).contains(&status) { Ok(v) } else { Err(GatewayError::Refused { status, body: v }) }
        })
        .await
    }
}

#[async_trait]
impl Gateway for GatewayBinding {
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
