//! Routes and the SQLite ledger port. Same HTTP contract as `workers/gateway`, so the API
//! side only swaps a Service Binding for a private-network URL.

use async_trait::async_trait;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use cairn_gateway_core::config::{GatewayConfig, KeyCheck};
use cairn_gateway_core::ledger::sqlite::MemSql;
use cairn_gateway_core::ledger::{Ledger, LedgerError, LedgerStatus, Reservation, ReserveRequest};
use cairn_gateway_core::openrouter::{OpenRouter, Price};
use cairn_gateway_core::service::{self, CompleteRequest, LedgerPort, ServiceError};
use cairn_http::HttpTransport;
use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// The ledger behind one mutex. Every `Ledger` call is synchronous, so check-then-write
/// can't interleave: the same guarantee as the Durable Object handling one request at a time.
pub struct SqliteLedger {
    sql: Mutex<MemSql>,
    cfg: Arc<GatewayConfig>,
    enable_after: Option<DateTime<Utc>>,
}

impl SqliteLedger {
    pub fn new(sql: MemSql, cfg: Arc<GatewayConfig>, enable_after: Option<DateTime<Utc>>) -> Self {
        Self { sql: Mutex::new(sql), cfg, enable_after }
    }

    fn with<R>(&self, f: impl FnOnce(&Ledger<'_, MemSql>) -> Result<R, LedgerError>) -> Result<R, LedgerError> {
        let sql = self.sql.lock().map_err(|_| LedgerError::Storage("ledger mutex poisoned".into()))?;
        let ledger = Ledger::new(&*sql, &self.cfg, self.enable_after)?;
        f(&ledger)
    }
}

#[async_trait]
impl LedgerPort for SqliteLedger {
    async fn reserve(&self, req: &ReserveRequest) -> Result<Reservation, LedgerError> {
        self.with(|l| l.reserve(req, Utc::now()))
    }
    async fn settle(&self, id: &str, actual_usd: Option<f64>) -> Result<(), LedgerError> {
        self.with(|l| l.settle(id, actual_usd, Utc::now()))
    }
    async fn status(&self) -> Result<LedgerStatus, LedgerError> {
        self.with(|l| l.status(Utc::now()))
    }
    async fn disable(&self, reason: &str) -> Result<(), LedgerError> {
        self.with(|l| l.disable(reason, Utc::now()))
    }
    async fn breaker_check(&self) -> Result<Option<String>, LedgerError> {
        self.with(|l| l.breaker_check(Utc::now()))
    }
    async fn mark_key_checked(&self) -> Result<(), LedgerError> {
        self.with(|l| l.mark_key_checked(Utc::now()))
    }
}

/// (fetched at, model → price). Ordering only; worst-case reservations use allowlist ceilings.
type PriceCache = Option<(DateTime<Utc>, HashMap<String, Price>)>;

pub struct GwState<T: HttpTransport> {
    ledger: Arc<dyn LedgerPort>,
    or: Arc<OpenRouter<T>>,
    cfg: Arc<GatewayConfig>,
    prices: Arc<Mutex<PriceCache>>,
}

impl<T: HttpTransport> Clone for GwState<T> {
    fn clone(&self) -> Self {
        Self { ledger: self.ledger.clone(), or: self.or.clone(), cfg: self.cfg.clone(), prices: self.prices.clone() }
    }
}

impl<T: HttpTransport + 'static> GwState<T> {
    pub fn new(ledger: impl LedgerPort + 'static, or: OpenRouter<T>, cfg: Arc<GatewayConfig>) -> Self {
        Self { ledger: Arc::new(ledger), or: Arc::new(or), cfg, prices: Arc::new(Mutex::new(None)) }
    }

    pub fn ai_gateway(&self) -> bool {
        self.or.upstream().is_ai_gateway()
    }

    async fn prices(&self) -> HashMap<String, Price> {
        let now = Utc::now();
        let cached = self
            .prices
            .lock()
            .ok()
            .and_then(|p| p.as_ref().filter(|(at, _)| now - *at < Duration::hours(1)).map(|(_, m)| m.clone()));
        if let Some(m) = cached {
            return m;
        }
        match self.or.prices().await {
            Ok(m) => {
                if let Ok(mut p) = self.prices.lock() {
                    *p = Some((now, m.clone()));
                }
                m
            }
            Err(e) => {
                tracing::warn!(error = %e, "price refresh failed; ordering by allowlist ceilings");
                HashMap::new()
            }
        }
    }
}

type ApiResult = Result<Json<Value>, (StatusCode, Json<Value>)>;

fn fail(e: ServiceError) -> (StatusCode, Json<Value>) {
    let status = StatusCode::from_u16(e.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (status, Json(json!({ "error": { "code": e.code, "message": e.message } })))
}

async fn complete<T: HttpTransport + 'static>(
    State(st): State<GwState<T>>,
    Json(req): Json<CompleteRequest>,
) -> ApiResult {
    let p = st.prices().await;
    let r = service::complete(&st.cfg, st.ledger.as_ref(), &st.or, &p, &req, Utc::now()).await.map_err(fail)?;
    Ok(Json(json!(r)))
}

async fn status<T: HttpTransport + 'static>(State(st): State<GwState<T>>) -> ApiResult {
    let s = st.ledger.status().await.map_err(|e| fail(e.into()))?;
    let key = st.or.key_info().await.ok();
    Ok(Json(json!({
        "ledger": s,
        "cap_today_usd": st.cfg.global.per_day_usd,
        "allowed_models": st.cfg.allow.iter().map(|m| m.model.clone()).collect::<Vec<_>>(),
        "upstream": if st.ai_gateway() { "ai-gateway-byok" } else { "direct" },
        "openrouter_usage_daily_usd": key.as_ref().and_then(|k| k.usage_daily),
        "openrouter_usage_monthly_usd": key.as_ref().and_then(|k| k.usage_monthly),
        "openrouter_limit_remaining_usd": key.as_ref().and_then(|k| k.limit_remaining),
    })))
}

/// Nightly (`builtin/reconcile`): compare the ledger with OpenRouter's own accounting.
async fn reconcile<T: HttpTransport + 'static>(State(st): State<GwState<T>>) -> ApiResult {
    let ours = st.ledger.status().await.map_err(|e| fail(e.into()))?.spent_today_usd;
    let key = match st.or.key_info().await {
        Ok(k) => k,
        // Through AI Gateway, /key may not be proxied: report, don't guess.
        Err(e) if st.cfg.global.key_check == KeyCheck::BestEffort => {
            return Ok(Json(
                json!({ "ok": null, "ledger_usd": ours, "reason": format!("key usage unavailable: {e}") }),
            ));
        }
        Err(e) => return Err(fail(ServiceError { status: 502, code: "openrouter", message: e.to_string() })),
    };
    let theirs = key.usage_daily.unwrap_or(0.0);
    let drift = theirs - ours;
    let ok = drift <= st.cfg.reconcile.tolerance_usd;
    if !ok {
        let reason = format!("reconcile: OpenRouter reports ${theirs:.4} today, ledger ${ours:.4}");
        st.ledger.disable(&reason).await.map_err(|e| fail(e.into()))?;
    }
    Ok(Json(json!({ "ok": ok, "ledger_usd": ours, "openrouter_usd": theirs, "drift_usd": drift })))
}

/// Tighten-only kill switch. There is deliberately no enable endpoint.
async fn disable<T: HttpTransport + 'static>(State(st): State<GwState<T>>) -> ApiResult {
    st.ledger.disable("requested via /v1/admin/disable").await.map_err(|e| fail(e.into()))?;
    Ok(Json(json!({ "disabled": true })))
}

pub fn router<T: HttpTransport + 'static>(st: GwState<T>) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/v1/complete", post(complete::<T>))
        .route("/v1/status", get(status::<T>))
        .route("/v1/reconcile", post(reconcile::<T>))
        .route("/v1/admin/disable", post(disable::<T>))
        .with_state(st)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use cairn_gateway_core::openrouter::Upstream;
    use cairn_http::FakeTransport;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn caps() -> Arc<GatewayConfig> {
        Arc::new(GatewayConfig::parse(include_str!("../../../config/gateway.toml")).unwrap())
    }

    fn state(path: &std::path::Path, enable_after: Option<DateTime<Utc>>) -> GwState<FakeTransport> {
        let up = Upstream::Direct { base: "https://or.example/api/v1".into(), key: "sk".into() };
        GwState::new(
            SqliteLedger::new(MemSql::open(path).unwrap(), caps(), enable_after),
            OpenRouter::new(Arc::new(FakeTransport::default()), up),
            caps(),
        )
    }

    async fn call(r: Router, method: &str, uri: &str) -> (StatusCode, Value) {
        let req = Request::builder().method(method).uri(uri).header("content-type", "application/json");
        let resp = r.oneshot(req.body(Body::from("{}")).unwrap()).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("cairn-gw-{name}-{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[tokio::test]
    async fn disable_persists_across_restarts_and_has_no_enable() {
        let path = tmp("disable");
        let (s, _) = call(router(state(&path, None)), "POST", "/v1/admin/disable").await;
        assert_eq!(s, StatusCode::OK);
        // "Restart": a new process opening the same file is still disabled.
        let st = state(&path, None);
        assert!(st.ledger.status().await.unwrap().disabled.is_some(), "disable is durable");
        for path in ["/v1/admin/enable", "/v1/enable"] {
            let (s, _) = call(router(st.clone()), "POST", path).await;
            assert_eq!(s, StatusCode::NOT_FOUND, "no enable endpoint");
        }
        // Only a later ENABLE_AFTER (operator edits the env file, recreates the container) lifts it.
        let later = Utc::now() + Duration::seconds(1);
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let st = state(&path, Some(later));
        assert!(st.ledger.status().await.unwrap().disabled.is_none(), "ENABLE_AFTER lifts it");
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn disabled_gateway_refuses_completions() {
        let path = tmp("refuse");
        let st = state(&path, None);
        st.ledger.disable("test").await.unwrap();
        let req = Request::builder()
            .method("POST")
            .uri("/v1/complete")
            .header("content-type", "application/json")
            .body(Body::from(json!({ "task": "t", "plugin": "p", "messages": [] }).to_string()))
            .unwrap();
        let resp = router(st).oneshot(req).await.unwrap();
        assert!(resp.status() == StatusCode::SERVICE_UNAVAILABLE || resp.status() == StatusCode::UNPROCESSABLE_ENTITY);
        let _ = std::fs::remove_file(&path);
    }
}
