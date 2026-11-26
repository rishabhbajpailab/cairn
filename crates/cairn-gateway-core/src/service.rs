//! The gateway's request flow, independent of Workers:
//!
//! key check (hourly) → order the task's models → for each: reserve (Ledger DO) →
//! OpenRouter call → settle → breaker check.

use crate::config::{GatewayConfig, KeyCheck};
use crate::ledger::{LedgerError, LedgerStatus, Reject, Reservation, ReserveRequest};
use crate::openrouter::{OpenRouter, OrError, Price, completion_body};
use crate::select::{self, Select};
use async_trait::async_trait;
use cairn_http::HttpTransport;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// The Ledger Durable Object, as seen from the gateway Worker.
#[async_trait]
pub trait LedgerPort: Send + Sync {
    async fn reserve(&self, req: &ReserveRequest) -> Result<Reservation, LedgerError>;
    async fn settle(&self, id: &str, actual_usd: Option<f64>) -> Result<(), LedgerError>;
    async fn status(&self) -> Result<LedgerStatus, LedgerError>;
    async fn disable(&self, reason: &str) -> Result<(), LedgerError>;
    async fn breaker_check(&self) -> Result<Option<String>, LedgerError>;
    async fn mark_key_checked(&self) -> Result<(), LedgerError>;
}

#[derive(Debug, Deserialize)]
pub struct CompleteRequest {
    pub task: String,
    pub plugin: String,
    pub run_key: String,
    pub models: Vec<String>,
    #[serde(default)]
    pub select: Select,
    #[serde(default = "default_io_ratio")]
    pub io_ratio: f64,
    pub messages: Value,
    #[serde(default)]
    pub response_schema: Option<Value>,
    pub max_tokens: u32,
    pub budget_per_run_usd: f64,
    pub budget_per_day_usd: f64,
    #[serde(default)]
    pub provider: serde_json::Map<String, Value>,
}

fn default_io_ratio() -> f64 {
    4.0
}

#[derive(Debug, Serialize, Deserialize)]
pub struct CompleteResponse {
    pub content: String,
    pub model: String,
    pub cost_usd: f64,
    pub reserved_usd: f64,
}

#[derive(Debug, thiserror::Error)]
#[error("{code}: {message}")]
pub struct ServiceError {
    pub status: u16,
    pub code: &'static str,
    pub message: String,
}

impl ServiceError {
    fn new(status: u16, code: &'static str, message: impl ToString) -> Self {
        Self { status, code, message: message.to_string() }
    }
}

impl From<LedgerError> for ServiceError {
    fn from(e: LedgerError) -> Self {
        match e {
            LedgerError::Reject(Reject::Disabled(r)) => Self::new(503, "disabled", r),
            LedgerError::Reject(r) => Self::new(402, "budget", r),
            LedgerError::Storage(m) => Self::new(500, "ledger", m),
        }
    }
}

const KEY_RECHECK: i64 = 60; // minutes

/// Refuse to run with a management/provisioning key or an uncapped key. Re-checked hourly.
/// A positive finding disables the gateway. If the check itself is unreachable, `policy`
/// decides: `Required` refuses (fail closed); `BestEffort` proceeds and retries next hour.
pub async fn ensure_key<T: HttpTransport>(
    ledger: &dyn LedgerPort,
    or: &OpenRouter<T>,
    policy: KeyCheck,
    now: DateTime<Utc>,
) -> Result<(), ServiceError> {
    let status = ledger.status().await?;
    if status.key_checked_at.is_some_and(|t| now - t < Duration::minutes(KEY_RECHECK)) {
        return Ok(());
    }
    let info = match or.key_info().await {
        Ok(i) => i,
        Err(e) if policy == KeyCheck::BestEffort => {
            tracing::warn!(error = %e, "key check unreachable; proceeding (key_check = best-effort)");
            ledger.mark_key_checked().await?;
            return Ok(());
        }
        Err(e) => return Err(ServiceError::new(502, "key_check_unavailable", e)),
    };
    let problem = if info.is_management_key || info.is_provisioning_key {
        Some("OPENROUTER_API_KEY is a management key; use a capped inference key")
    } else if info.limit.is_none() {
        Some("OPENROUTER_API_KEY has no credit limit; set one in the OpenRouter dashboard")
    } else {
        None
    };
    if let Some(p) = problem {
        ledger.disable(p).await?;
        return Err(ServiceError::new(503, "key_rejected", p));
    }
    ledger.mark_key_checked().await?;
    Ok(())
}

pub async fn complete<T: HttpTransport>(
    cfg: &GatewayConfig,
    ledger: &dyn LedgerPort,
    or: &OpenRouter<T>,
    prices: &HashMap<String, Price>,
    req: &CompleteRequest,
    now: DateTime<Utc>,
) -> Result<CompleteResponse, ServiceError> {
    if let Some(reason) = ledger.status().await?.disabled {
        return Err(ServiceError::new(503, "disabled", reason));
    }
    ensure_key(ledger, or, cfg.global.key_check, now).await?;
    let metadata = serde_json::json!({ "app": "cairn", "task": req.task, "plugin": req.plugin });

    let est_prompt_tokens = select::estimate_prompt_tokens(&req.messages);
    let order = select::order_models(cfg, prices, &req.models, req.select, req.io_ratio);
    if order.is_empty() {
        return Err(ServiceError::new(403, "no_allowed_model", "none of the requested models are allowlisted"));
    }

    let mut last_err = String::new();
    for cand in order {
        let worst = select::worst_case_usd(&cand, est_prompt_tokens, req.max_tokens);
        let reservation = ledger
            .reserve(&ReserveRequest {
                task: req.task.clone(),
                plugin: req.plugin.clone(),
                run_key: req.run_key.clone(),
                model: cand.model.clone(),
                worst_usd: worst,
                task_per_run_usd: req.budget_per_run_usd,
                task_per_day_usd: req.budget_per_day_usd,
            })
            .await;
        let reservation = match reservation {
            Ok(r) => r,
            Err(e) => {
                ledger.breaker_check().await?;
                return Err(e.into());
            }
        };

        let body = completion_body(
            &cand.model,
            &req.messages,
            req.response_schema.as_ref(),
            req.max_tokens,
            (cand.max_prompt_per_m, cand.max_completion_per_m),
            &req.provider,
            &cfg.global.providers_only,
        );
        match or.complete(&cand.model, &body, Some(&metadata)).await {
            Ok(c) => {
                let actual =
                    c.cost_usd.or_else(|| select::cost_from_tokens(&cand, c.prompt_tokens, c.completion_tokens));
                ledger.settle(&reservation.id, actual).await?;
                ledger.breaker_check().await?;
                return Ok(CompleteResponse {
                    content: c.content,
                    model: c.model,
                    cost_usd: actual.unwrap_or(reservation.worst_usd),
                    reserved_usd: reservation.worst_usd,
                });
            }
            Err(OrError::Status { status: 429, body }) => {
                // Rate or spend limit upstream (AI Gateway spend limit, OpenRouter, provider).
                // It applies to every model, so stop here instead of trying the next one.
                ledger.settle(&reservation.id, Some(0.0)).await?;
                ledger.breaker_check().await?;
                return Err(ServiceError::new(429, "upstream_limited", body));
            }
            Err(e) => {
                // A 4xx means the request was refused before running: release it. Anything else
                // may have run, so the worst case stays charged.
                let refused = matches!(&e, OrError::Status { status, .. } if *status < 500);
                ledger.settle(&reservation.id, refused.then_some(0.0)).await?;
                tracing::warn!(model = %cand.model, error = %e, "model attempt failed; trying next in task list");
                last_err = e.to_string();
            }
        }
    }
    Err(ServiceError::new(502, "all_models_failed", last_err))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ledger::Ledger;
    use crate::ledger::sqlite::MemSql;
    use crate::openrouter::Upstream;
    use cairn_http::FakeTransport;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    /// LedgerPort over an in-memory SQLite, mirroring the Durable Object.
    struct TestLedger {
        sql: Mutex<MemSql>,
        cfg: GatewayConfig,
    }

    // rusqlite::Connection is Send but not Sync; the Mutex provides Sync for the trait bound.
    impl TestLedger {
        fn with<R>(&self, f: impl FnOnce(&Ledger<'_, MemSql>) -> R) -> R {
            let sql = self.sql.lock().unwrap();
            let l = Ledger::new(&*sql, &self.cfg, None).unwrap();
            f(&l)
        }
    }

    #[async_trait]
    impl LedgerPort for TestLedger {
        async fn reserve(&self, req: &ReserveRequest) -> Result<Reservation, LedgerError> {
            self.with(|l| l.reserve(req, Utc::now()))
        }
        async fn settle(&self, id: &str, a: Option<f64>) -> Result<(), LedgerError> {
            self.with(|l| l.settle(id, a, Utc::now()))
        }
        async fn status(&self) -> Result<LedgerStatus, LedgerError> {
            self.with(|l| l.status(Utc::now()))
        }
        async fn disable(&self, r: &str) -> Result<(), LedgerError> {
            self.with(|l| l.disable(r, Utc::now()))
        }
        async fn breaker_check(&self) -> Result<Option<String>, LedgerError> {
            self.with(|l| l.breaker_check(Utc::now()))
        }
        async fn mark_key_checked(&self) -> Result<(), LedgerError> {
            self.with(|l| l.mark_key_checked(Utc::now()))
        }
    }

    fn cfg() -> GatewayConfig {
        GatewayConfig::parse(include_str!("../../../config/gateway.toml")).unwrap()
    }

    fn request() -> CompleteRequest {
        serde_json::from_value(json!({
            "task": "nightly-digest", "plugin": "digest", "run_key": "k",
            "models": ["anthropic/claude-haiku-4.5", "evil/unlisted"],
            "select": "cheapest", "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 200, "budget_per_run_usd": 0.05, "budget_per_day_usd": 0.10
        }))
        .unwrap()
    }

    fn direct() -> Upstream {
        Upstream::Direct { base: crate::openrouter::DIRECT_BASE.into(), key: "sk".into() }
    }

    fn key_ok() -> cairn_http::HttpResponse {
        FakeTransport::json(200, json!({ "data": { "limit": 5.0, "is_management_key": false } }))
    }

    #[tokio::test]
    async fn happy_path_reserves_calls_and_settles_actual_cost() {
        let ledger = TestLedger { sql: Mutex::new(MemSql::new()), cfg: cfg() };
        let t = Arc::new(FakeTransport::with_responses(vec![
            key_ok(),
            FakeTransport::json(
                200,
                json!({
                    "model": "anthropic/claude-haiku-4.5",
                    "choices": [{ "message": { "content": "{\"ok\":true}" } }],
                    "usage": { "cost": 0.0003, "prompt_tokens": 10, "completion_tokens": 5 }
                }),
            ),
        ]));
        let or = OpenRouter::new(t.clone(), direct());
        let r = complete(&ledger.cfg.clone(), &ledger, &or, &HashMap::new(), &request(), Utc::now()).await.unwrap();
        assert_eq!(r.cost_usd, 0.0003);
        let body = t.sent()[1].body_json().unwrap();
        assert_eq!(body["model"], "anthropic/claude-haiku-4.5", "unlisted model never tried");
        assert!((ledger.status().await.unwrap().spent_today_usd - 0.0003).abs() < 1e-12);
    }

    #[tokio::test]
    async fn management_key_disables_gateway() {
        let ledger = TestLedger { sql: Mutex::new(MemSql::new()), cfg: cfg() };
        let t = Arc::new(FakeTransport::with_responses(vec![FakeTransport::json(
            200,
            json!({ "data": { "limit": null, "is_management_key": true } }),
        )]));
        let or = OpenRouter::new(t, direct());
        let e = complete(&ledger.cfg.clone(), &ledger, &or, &HashMap::new(), &request(), Utc::now()).await.unwrap_err();
        assert_eq!(e.code, "key_rejected");
        assert!(ledger.status().await.unwrap().disabled.is_some());
    }

    #[tokio::test]
    async fn refused_request_is_released_not_charged() {
        let ledger = TestLedger { sql: Mutex::new(MemSql::new()), cfg: cfg() };
        let t = Arc::new(FakeTransport::with_responses(vec![
            key_ok(),
            FakeTransport::json(400, json!({ "error": { "message": "bad request" } })),
        ]));
        let or = OpenRouter::new(t, direct());
        let mut req = request();
        req.models = vec!["anthropic/claude-haiku-4.5".into()];
        let e = complete(&ledger.cfg.clone(), &ledger, &or, &HashMap::new(), &req, Utc::now()).await.unwrap_err();
        assert_eq!(e.code, "all_models_failed");
        assert_eq!(ledger.status().await.unwrap().spent_today_usd, 0.0);
    }

    fn completion_ok() -> cairn_http::HttpResponse {
        FakeTransport::json(
            200,
            json!({
                "choices": [{ "message": { "content": "{}" } }],
                "usage": { "cost": 0.0001 }
            }),
        )
    }

    #[tokio::test]
    async fn unreachable_key_check_fails_closed_by_default() {
        let ledger = TestLedger { sql: Mutex::new(MemSql::new()), cfg: cfg() };
        assert_eq!(ledger.cfg.global.key_check, KeyCheck::Required);
        let t = Arc::new(FakeTransport::with_responses(vec![FakeTransport::json(404, json!({}))]));
        let or = OpenRouter::new(t, direct());
        let e = complete(&ledger.cfg.clone(), &ledger, &or, &HashMap::new(), &request(), Utc::now()).await.unwrap_err();
        assert_eq!(e.code, "key_check_unavailable");
        assert!(ledger.status().await.unwrap().disabled.is_none(), "unreachable is not the same as a bad key");
    }

    #[tokio::test]
    async fn best_effort_key_check_proceeds_and_sends_metadata() {
        let mut c = cfg();
        c.global.key_check = KeyCheck::BestEffort;
        let ledger = TestLedger { sql: Mutex::new(MemSql::new()), cfg: c };
        let t = Arc::new(FakeTransport::with_responses(vec![FakeTransport::json(404, json!({})), completion_ok()]));
        let up = Upstream::AiGateway { base: "https://aig/openrouter/v1".into(), token: "t".into(), alias: None };
        let or = OpenRouter::new(t.clone(), up);
        complete(&ledger.cfg.clone(), &ledger, &or, &HashMap::new(), &request(), Utc::now()).await.unwrap();
        let meta = t.sent()[1].headers.iter().find(|(k, _)| k == "cf-aig-metadata").map(|(_, v)| v.clone()).unwrap();
        assert!(meta.contains("nightly-digest") && meta.contains("digest"));
        assert!(ledger.status().await.unwrap().key_checked_at.is_some(), "retry throttled to hourly");
    }

    #[tokio::test]
    async fn upstream_429_stops_and_charges_nothing() {
        let ledger = TestLedger { sql: Mutex::new(MemSql::new()), cfg: cfg() };
        let t = Arc::new(FakeTransport::with_responses(vec![
            key_ok(),
            FakeTransport::json(429, json!({ "error": "spend limit" })),
        ]));
        let or = OpenRouter::new(t.clone(), direct());
        let mut req = request();
        req.models = vec!["anthropic/claude-haiku-4.5".into(), "google/gemini-2.5-flash".into()];
        let e = complete(&ledger.cfg.clone(), &ledger, &or, &HashMap::new(), &req, Utc::now()).await.unwrap_err();
        assert_eq!(e.code, "upstream_limited");
        assert_eq!(t.sent().len(), 2, "second model not attempted after a 429");
        assert_eq!(ledger.status().await.unwrap().spent_today_usd, 0.0);
    }
}
