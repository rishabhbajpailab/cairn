//! cairn-gateway Worker (ADR 0003, docs/spend-control.md).
//!
//! - The only Worker with a spend credential bound: `CF_AIG_TOKEN` (AI Gateway + BYOK, the
//!   OpenRouter key itself stays in Cloudflare Secrets Store), or `OPENROUTER_API_KEY` for
//!   local dev only. Never both.
//! - No public route: `workers_dev = false`, no `routes`; reachable only through the API
//!   Worker's `GATEWAY` Service Binding.
//! - Caps are compiled in (`config/gateway.toml`); the ledger lives in [`LedgerDo`].
//! - Code can only tighten: there's a disable endpoint and no enable endpoint.

mod caps;
mod ledger_do;
mod transport;

pub use ledger_do::LedgerDo;

use async_trait::async_trait;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use cairn_gateway_core::config::KeyCheck;
use cairn_gateway_core::ledger::{LedgerError, LedgerStatus, Reject, Reservation, ReserveRequest};
use cairn_gateway_core::openrouter::{DIRECT_BASE, OpenRouter, Price, Upstream};
use cairn_gateway_core::service::{self, CompleteRequest, LedgerPort, ServiceError};
use chrono::{DateTime, Duration, Utc};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;
use tower_service::Service;
use transport::WorkerFetch;
use worker::send::{SendFuture, SendWrapper};
use worker::{Context, Env, HttpRequest, RequestInit, Stub, event};

/// Ledger DO stub as a `LedgerPort`.
struct DoLedger(SendWrapper<Stub>);

impl DoLedger {
    async fn call<T: DeserializeOwned>(&self, path: &str, body: Value) -> Result<T, LedgerError> {
        SendFuture::new(async {
            let storage = |e: worker::Error| LedgerError::Storage(e.to_string());
            let headers = worker::Headers::new();
            headers.set("content-type", "application/json").map_err(storage)?;
            let mut init = RequestInit::new();
            init.with_method(worker::Method::Post).with_headers(headers).with_body(Some(body.to_string().into()));
            let req = worker::Request::new_with_init(&format!("https://ledger{path}"), &init).map_err(storage)?;
            let mut resp = self.0.fetch_with_request(req).await.map_err(storage)?;
            let status = resp.status_code();
            let v: Value = resp.json().await.map_err(storage)?;
            if (200..300).contains(&status) {
                serde_json::from_value(v).map_err(|e| LedgerError::Storage(e.to_string()))
            } else if let Some(r) =
                v.pointer("/error/reject").and_then(|r| serde_json::from_value::<Reject>(r.clone()).ok())
            {
                Err(LedgerError::Reject(r))
            } else {
                Err(LedgerError::Storage(v.to_string()))
            }
        })
        .await
    }
}

#[async_trait]
impl LedgerPort for DoLedger {
    async fn reserve(&self, req: &ReserveRequest) -> Result<Reservation, LedgerError> {
        self.call("/reserve", json!(req)).await
    }
    async fn settle(&self, id: &str, actual_usd: Option<f64>) -> Result<(), LedgerError> {
        self.call::<Value>("/settle", json!({ "id": id, "actual_usd": actual_usd })).await.map(|_| ())
    }
    async fn status(&self) -> Result<LedgerStatus, LedgerError> {
        self.call("/status", json!({})).await
    }
    async fn disable(&self, reason: &str) -> Result<(), LedgerError> {
        self.call::<Value>("/disable", json!({ "reason": reason })).await.map(|_| ())
    }
    async fn breaker_check(&self) -> Result<Option<String>, LedgerError> {
        let v: Value = self.call("/breaker", json!({})).await?;
        Ok(v.get("tripped").and_then(Value::as_str).map(str::to_owned))
    }
    async fn mark_key_checked(&self) -> Result<(), LedgerError> {
        self.call::<Value>("/key-checked", json!({})).await.map(|_| ())
    }
}

#[derive(Clone)]
struct GwState {
    ledger: Arc<DoLedger>,
    or: Arc<OpenRouter<WorkerFetch>>,
}

/// (fetched at, model → price)
type PriceCache = Option<(DateTime<Utc>, HashMap<String, Price>)>;

thread_local! {
    /// Per-isolate price cache (ordering only; worst-case uses allowlist ceilings).
    static PRICES: RefCell<PriceCache> = const { RefCell::new(None) };
}

async fn prices(or: &OpenRouter<WorkerFetch>) -> HashMap<String, Price> {
    let now = Utc::now();
    let cached =
        PRICES.with(|p| p.borrow().as_ref().filter(|(at, _)| now - *at < Duration::hours(1)).map(|(_, m)| m.clone()));
    if let Some(m) = cached {
        return m;
    }
    match or.prices().await {
        Ok(m) => {
            PRICES.with(|p| *p.borrow_mut() = Some((now, m.clone())));
            m
        }
        Err(e) => {
            tracing::warn!(error = %e, "price refresh failed; ordering by allowlist ceilings");
            HashMap::new()
        }
    }
}

type ApiResult = Result<Json<Value>, (StatusCode, Json<Value>)>;

fn fail(e: ServiceError) -> (StatusCode, Json<Value>) {
    let status = StatusCode::from_u16(e.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (status, Json(json!({ "error": { "code": e.code, "message": e.message } })))
}

async fn complete(State(st): State<GwState>, Json(req): Json<CompleteRequest>) -> ApiResult {
    SendFuture::new(async {
        let cfg =
            caps::config().map_err(|e| fail(ServiceError { status: 500, code: "config", message: e.to_string() }))?;
        let p = prices(&st.or).await;
        let r = service::complete(&cfg, st.ledger.as_ref(), &st.or, &p, &req, Utc::now()).await.map_err(fail)?;
        Ok(Json(json!(r)))
    })
    .await
}

async fn status(State(st): State<GwState>) -> ApiResult {
    SendFuture::new(async {
        let s = st.ledger.status().await.map_err(|e| fail(e.into()))?;
        let key = st.or.key_info().await.ok();
        let cfg = caps::config().ok();
        Ok(Json(json!({
            "ledger": s,
            "cap_today_usd": cfg.as_ref().map(|c| c.global.per_day_usd),
            "allowed_models": cfg.as_ref().map(|c| c.allow.iter().map(|m| m.model.clone()).collect::<Vec<_>>()),
            "upstream": if st.or.upstream().is_ai_gateway() { "ai-gateway-byok" } else { "direct" },
            "openrouter_usage_daily_usd": key.as_ref().and_then(|k| k.usage_daily),
            "openrouter_usage_monthly_usd": key.as_ref().and_then(|k| k.usage_monthly),
            "openrouter_limit_remaining_usd": key.as_ref().and_then(|k| k.limit_remaining),
        })))
    })
    .await
}

/// Nightly (`builtin/reconcile`): compare the ledger with OpenRouter's own accounting.
async fn reconcile(State(st): State<GwState>) -> ApiResult {
    SendFuture::new(async {
        let cfg =
            caps::config().map_err(|e| fail(ServiceError { status: 500, code: "config", message: e.to_string() }))?;
        let ours = st.ledger.status().await.map_err(|e| fail(e.into()))?.spent_today_usd;
        let key = match st.or.key_info().await {
            Ok(k) => k,
            // Through AI Gateway, /key may not be proxied: report, don't guess.
            Err(e) if cfg.global.key_check == KeyCheck::BestEffort => {
                return Ok(Json(
                    json!({ "ok": null, "ledger_usd": ours, "reason": format!("key usage unavailable: {e}") }),
                ));
            }
            Err(e) => return Err(fail(ServiceError { status: 502, code: "openrouter", message: e.to_string() })),
        };
        let theirs = key.usage_daily.unwrap_or(0.0);
        let drift = theirs - ours;
        let ok = drift <= cfg.reconcile.tolerance_usd;
        if !ok {
            let reason = format!("reconcile: OpenRouter reports ${theirs:.4} today, ledger ${ours:.4}");
            st.ledger.disable(&reason).await.map_err(|e| fail(e.into()))?;
        }
        Ok(Json(json!({ "ok": ok, "ledger_usd": ours, "openrouter_usd": theirs, "drift_usd": drift })))
    })
    .await
}

/// Tighten-only kill switch. There is deliberately no enable endpoint.
async fn disable(State(st): State<GwState>) -> ApiResult {
    SendFuture::new(async {
        st.ledger.disable("requested via /v1/admin/disable").await.map_err(|e| fail(e.into()))?;
        Ok(Json(json!({ "disabled": true })))
    })
    .await
}

fn var(env: &Env, name: &str) -> Option<String> {
    env.var(name).ok().map(|v| v.to_string()).filter(|s| !s.trim().is_empty())
}

/// Production: `CF_AIG_TOKEN` + `AIG_BASE_URL` (AI Gateway with the OpenRouter key stored via
/// BYOK). Local dev: `OPENROUTER_API_KEY` direct. Having both bound is a misconfiguration.
fn upstream(env: &Env) -> worker::Result<Upstream> {
    let aig = env.secret("CF_AIG_TOKEN").ok().map(|s| s.to_string());
    let direct = env.secret("OPENROUTER_API_KEY").ok().map(|s| s.to_string());
    match (aig, direct) {
        (Some(_), Some(_)) => Err(worker::Error::RustError(
            "both CF_AIG_TOKEN and OPENROUTER_API_KEY are bound; with BYOK the raw key must not be on any Worker"
                .into(),
        )),
        (Some(token), None) => Ok(Upstream::AiGateway {
            base: var(env, "AIG_BASE_URL").ok_or_else(|| worker::Error::RustError("AIG_BASE_URL var".into()))?,
            token,
            alias: var(env, "AIG_BYOK_ALIAS"),
        }),
        (None, Some(key)) => Ok(Upstream::Direct { base: DIRECT_BASE.into(), key }),
        (None, None) => Err(worker::Error::RustError("no upstream credential: set CF_AIG_TOKEN".into())),
    }
}

#[event(fetch)]
async fn fetch(req: HttpRequest, env: Env, _ctx: Context) -> worker::Result<axum::http::Response<axum::body::Body>> {
    let stub = env.durable_object("LEDGER")?.id_from_name("ledger")?.get_stub()?;
    let st = GwState {
        ledger: Arc::new(DoLedger(SendWrapper::new(stub))),
        or: Arc::new(OpenRouter::new(Arc::new(WorkerFetch), upstream(&env)?)),
    };
    let mut router = Router::new()
        .route("/v1/complete", post(complete))
        .route("/v1/status", get(status))
        .route("/v1/reconcile", post(reconcile))
        .route("/v1/admin/disable", post(disable))
        .with_state(st);
    Ok(router.call(req).await?)
}
