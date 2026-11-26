//! Ports: what the app needs from the platform. The API Worker implements these with
//! Cloudflare bindings (Queue, Service Binding, Durable Objects); tests use the in-memory
//! versions below.

use async_trait::async_trait;
use cairn_domain::{Capture, Id, Item};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Mutex;

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct PortError(pub String);

/// One queued capture. The item is planned at enqueue time (pure resolution), so the
/// consumer is idempotent: re-delivery re-creates the same ids and treats conflicts as done.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureMsg {
    pub capture: Capture,
    pub item: Option<Item>,
    pub error: Option<String>,
}

/// Durable spool for raw captures (Cloudflare Queue). Invariant 1: once `enqueue` returns Ok,
/// the capture will reach the store even if the database is paused or unreachable.
#[async_trait]
pub trait Spool: Send + Sync {
    async fn enqueue(&self, msg: &CaptureMsg) -> Result<(), PortError>;
}

/// Live-update fan-out to open browsers (Hub Durable Object).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ChangeEvent {
    pub kind: String,
    pub id: Id,
}

#[async_trait]
pub trait Hub: Send + Sync {
    async fn publish(&self, ev: &ChangeEvent);
}

/// Tell the Scheduler Durable Object to recompute its next alarm (after task/reminder writes).
#[async_trait]
pub trait SchedulerPort: Send + Sync {
    async fn rearm(&self);
}

// ───────────── gateway (Service Binding to cairn-gateway) ─────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompleteRequest {
    pub task: String,
    pub plugin: String,
    pub run_key: String,
    pub models: Vec<String>,
    pub select: String,
    pub io_ratio: f64,
    pub messages: Value,
    pub response_schema: Option<Value>,
    pub max_tokens: u32,
    pub budget_per_run_usd: f64,
    pub budget_per_day_usd: f64,
    #[serde(default)]
    pub provider: serde_json::Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompleteResponse {
    pub content: String,
    pub model: String,
    pub cost_usd: f64,
    pub reserved_usd: f64,
}

#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    #[error("gateway unreachable: {0}")]
    Unreachable(String),
    /// 402 budget, 503 disabled, 403 model not allowed, 502 all models failed
    #[error("gateway refused ({status}): {body}")]
    Refused { status: u16, body: Value },
}

#[async_trait]
pub trait Gateway: Send + Sync {
    async fn complete(&self, req: &CompleteRequest) -> Result<CompleteResponse, GatewayError>;
    /// Tighten-only: there is no `enable` counterpart anywhere in the app.
    async fn disable(&self) -> Result<(), GatewayError>;
    async fn status(&self) -> Result<Value, GatewayError>;
}

// ───────────── in-memory implementations for tests ─────────────

#[derive(Default)]
pub struct VecSpool(pub Mutex<Vec<CaptureMsg>>);

impl VecSpool {
    pub fn drain(&self) -> Vec<CaptureMsg> {
        self.0.lock().map(|mut v| std::mem::take(&mut *v)).unwrap_or_default()
    }
}

#[async_trait]
impl Spool for VecSpool {
    async fn enqueue(&self, msg: &CaptureMsg) -> Result<(), PortError> {
        self.0.lock().map_err(|e| PortError(e.to_string()))?.push(msg.clone());
        Ok(())
    }
}

#[derive(Default)]
pub struct RecordingHub(pub Mutex<Vec<ChangeEvent>>);

#[async_trait]
impl Hub for RecordingHub {
    async fn publish(&self, ev: &ChangeEvent) {
        if let Ok(mut v) = self.0.lock() {
            v.push(ev.clone());
        }
    }
}

#[derive(Default)]
pub struct CountingScheduler(pub std::sync::atomic::AtomicUsize);

#[async_trait]
impl SchedulerPort for CountingScheduler {
    async fn rearm(&self) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}
