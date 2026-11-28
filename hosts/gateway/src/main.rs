//! cairn-gateway on the VPS (ADR 0014, docs/spend-control.md).
//!
//! - The only process with a spend credential: `CF_AIG_TOKEN` (AI Gateway + BYOK), or
//!   `OPENROUTER_API_KEY` for local dev only. Never both.
//! - No published port: compose puts it on a private network shared only with `cairn`.
//! - Caps are compiled in (`config/gateway.toml`). The ledger is a SQLite file on this
//!   container's own volume, behind a mutex, so reserve/settle are serialized like the DO.
//! - Code can only tighten: a disable endpoint and no enable endpoint. Lifting a disable
//!   means a later `ENABLE_AFTER` in this container's env file and recreating it.

mod app;

use cairn_gateway_core::config::GatewayConfig;
use cairn_gateway_core::ledger::sqlite::MemSql;
use cairn_gateway_core::openrouter::{OpenRouter, Upstream};
use cairn_http::native::ReqwestTransport;
use chrono::{DateTime, Utc};
use std::sync::Arc;

const CAPS: &str = include_str!("../../../config/gateway.toml");

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

#[tokio::main]
async fn main() -> Result<(), String> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    // Invariant 5, other direction: the gateway never holds database credentials.
    for name in ["SURREAL_USER", "SURREAL_PASS", "CAIRN_TOKENS"] {
        if env(name).is_some() {
            return Err(format!("{name} is set on cairn-gateway; it belongs only to cairn"));
        }
    }
    let cfg = Arc::new(GatewayConfig::parse(CAPS).map_err(|e| format!("config/gateway.toml: {e}"))?);
    let enable_after: Option<DateTime<Utc>> = match env("ENABLE_AFTER") {
        Some(raw) => Some(
            DateTime::parse_from_rfc3339(raw.trim()).map_err(|e| format!("ENABLE_AFTER: {e}"))?.with_timezone(&Utc),
        ),
        None => None,
    };
    let upstream = Upstream::from_credentials(
        env("CF_AIG_TOKEN"),
        env("AIG_BASE_URL"),
        env("AIG_BYOK_ALIAS"),
        env("OPENROUTER_API_KEY"),
    )?;
    let path = env("LEDGER_PATH").unwrap_or_else(|| "/data/ledger.sqlite".into());
    let sql = MemSql::open(std::path::Path::new(&path)).map_err(|e| format!("ledger {path}: {e}"))?;
    let http = Arc::new(ReqwestTransport::new(std::time::Duration::from_secs(120)).map_err(|e| e.to_string())?);
    let state =
        app::GwState::new(app::SqliteLedger::new(sql, cfg.clone(), enable_after), OpenRouter::new(http, upstream), cfg);

    let bind = env("GATEWAY_BIND").unwrap_or_else(|| "0.0.0.0:8788".into());
    let listener = tokio::net::TcpListener::bind(&bind).await.map_err(|e| format!("bind {bind}: {e}"))?;
    tracing::info!(%bind, ledger = %path, upstream = if state.ai_gateway() { "ai-gateway-byok" } else { "direct" }, "cairn-gateway listening");
    axum::serve(listener, app::router(state))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .map_err(|e| e.to_string())
}
