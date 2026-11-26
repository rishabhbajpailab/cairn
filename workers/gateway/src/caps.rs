//! Caps compiled into this Worker. Changing them requires `wrangler deploy`.

use cairn_gateway_core::config::GatewayConfig;
use chrono::{DateTime, Utc};
use std::cell::OnceCell;
use std::sync::Arc;
use worker::{Env, Error, Result};

const RAW: &str = include_str!("../../../config/gateway.toml");

thread_local! {
    static CFG: OnceCell<Arc<GatewayConfig>> = const { OnceCell::new() };
}

pub fn config() -> Result<Arc<GatewayConfig>> {
    CFG.with(|c| {
        if let Some(v) = c.get() {
            return Ok(v.clone());
        }
        let v = Arc::new(GatewayConfig::parse(RAW).map_err(|e| Error::RustError(format!("config/gateway.toml: {e}")))?);
        let _ = c.set(v.clone());
        Ok(v)
    })
}

/// `ENABLE_AFTER` (RFC 3339) from wrangler `[vars]`. Bumping it and redeploying is the only
/// way to lift a disable (kill switch, breaker, reconcile drift, key rejection).
pub fn enable_after(env: &Env) -> Option<DateTime<Utc>> {
    let raw = env.var("ENABLE_AFTER").ok()?.to_string();
    DateTime::parse_from_rfc3339(raw.trim()).ok().map(|t| t.with_timezone(&Utc))
}
