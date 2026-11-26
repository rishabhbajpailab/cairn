//! Minimal OpenRouter client over [`HttpTransport`]. Lives ONLY in the gateway (CLAUDE.md
//! invariant 5). Two upstream modes ([`Upstream`]):
//!
//! - **AI Gateway + BYOK (production).** Requests go to Cloudflare AI Gateway's OpenRouter
//!   endpoint with `cf-aig-authorization`, and *no* provider `Authorization` header, so AI
//!   Gateway injects the OpenRouter key stored in Secrets Store. The Worker never sees the key.
//! - **Direct (local dev only).** Bearer key straight to OpenRouter.
//!
//! The model price table always comes from OpenRouter's public `/models` endpoint, which
//! needs no credentials.
//!
//! Field names follow the OpenRouter docs as of Nov 2026: `/api/v1/key`, `/api/v1/models`,
//! `/api/v1/chat/completions` with `provider` routing (`sort`, `only`, `max_price`,
//! `data_collection`, `zdr`, `require_parameters`, `allow_fallbacks`). Re-check them when
//! upgrading, since response shapes change over time.

use cairn_http::{HttpRequest, HttpTransport};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;

pub const DIRECT_BASE: &str = "https://openrouter.ai/api/v1";

/// Public, unauthenticated price list.
pub const PUBLIC_MODELS_URL: &str = "https://openrouter.ai/api/v1/models";

#[derive(Debug, Clone)]
pub enum Upstream {
    /// Cloudflare AI Gateway with the OpenRouter key stored via BYOK.
    /// `base` = `https://gateway.ai.cloudflare.com/v1/{account_id}/{gateway_id}/openrouter/v1`.
    AiGateway {
        base: String,
        /// AI Gateway auth token (`cf-aig-authorization`). Can spend through the stored key,
        /// so it is a spend credential: bound only to the gateway Worker.
        token: String,
        /// `cf-aig-byok-alias`: pick Cairn's own stored OpenRouter key (recommended when the
        /// gateway is shared with other apps). `None` = the `default` alias.
        alias: Option<String>,
    },
    /// Direct to OpenRouter with a raw key. For `wrangler dev` without an AI Gateway.
    Direct { base: String, key: String },
}

impl Upstream {
    fn base(&self) -> &str {
        match self {
            Upstream::AiGateway { base, .. } | Upstream::Direct { base, .. } => base.trim_end_matches('/'),
        }
    }

    fn authorize(&self, req: HttpRequest, metadata: Option<&Value>) -> HttpRequest {
        match self {
            Upstream::AiGateway { token, alias, .. } => {
                // No provider Authorization header: AI Gateway substitutes the stored key
                // only when it's absent.
                // `cf-aig-no-wholesale`: if no stored key matches (say, a mistyped alias), fail
                // with error 2044 instead of falling back to Unified Billing, which would bill
                // Cloudflare credits and bypass the OpenRouter key's limit.
                let mut r = req
                    .header("cf-aig-authorization", format!("Bearer {token}"))
                    .header("cf-aig-no-wholesale", "true");
                if let Some(a) = alias {
                    r = r.header("cf-aig-byok-alias", a.as_str());
                }
                if let Some(m) = metadata {
                    // Lets AI Gateway spend-limit rules and analytics split by task/plugin.
                    r = r.header("cf-aig-metadata", m.to_string());
                }
                r
            }
            Upstream::Direct { key, .. } => req.bearer(key),
        }
    }

    pub fn is_ai_gateway(&self) -> bool {
        matches!(self, Upstream::AiGateway { .. })
    }
}

pub struct OpenRouter<T: HttpTransport> {
    http: Arc<T>,
    upstream: Upstream,
}

#[derive(Debug, Deserialize)]
pub struct KeyInfo {
    pub limit: Option<f64>,
    #[serde(default)]
    pub limit_remaining: Option<f64>,
    #[serde(default)]
    pub limit_reset: Option<String>,
    #[serde(default)]
    pub is_management_key: bool,
    #[serde(default)]
    pub is_provisioning_key: bool,
    #[serde(default)]
    pub usage_daily: Option<f64>,
    #[serde(default)]
    pub usage_monthly: Option<f64>,
}

/// USD per token (OpenRouter returns decimal strings).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Price {
    pub prompt: f64,
    pub completion: f64,
}

#[derive(Debug)]
pub struct Completion {
    pub content: String,
    pub model: String,
    pub cost_usd: Option<f64>,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
}

#[derive(Debug, thiserror::Error)]
pub enum OrError {
    #[error("http: {0}")]
    Http(#[from] cairn_http::HttpError),
    #[error("openrouter returned {status}: {body}")]
    Status { status: u16, body: String },
    #[error("unexpected response: {0}")]
    Shape(String),
}

/// Request body for one completion with strict routing. Pure, so it's unit-tested.
/// Fixed by the gateway: `sort: price`, `data_collection: deny`, `require_parameters: true`,
/// `max_price` from the allowlist ceiling. Task settings may only TIGHTEN (`zdr: true`,
/// `allow_fallbacks: false`); anything else in `provider_extra` is ignored.
pub fn completion_body(
    model: &str,
    messages: &Value,
    response_schema: Option<&Value>,
    max_tokens: u32,
    max_price_per_m: (f64, f64),
    provider_extra: &serde_json::Map<String, Value>,
    providers_only: &[String],
) -> Value {
    let mut provider = serde_json::Map::new();
    provider.insert("sort".into(), json!("price"));
    provider.insert("max_price".into(), json!({ "prompt": max_price_per_m.0, "completion": max_price_per_m.1 }));
    provider.insert("data_collection".into(), json!("deny"));
    provider.insert("require_parameters".into(), json!(true));
    for (k, v) in provider_extra {
        match (k.as_str(), v) {
            ("zdr", Value::Bool(true)) | ("allow_fallbacks", Value::Bool(false)) => {
                provider.insert(k.clone(), v.clone());
            }
            _ => tracing::debug!(key = %k, "ignoring provider setting that is not tighten-only"),
        }
    }
    if !providers_only.is_empty() {
        provider.insert("only".into(), json!(providers_only));
    }
    let mut body = json!({
        "model": model,
        "messages": messages,
        "max_tokens": max_tokens,
        "provider": provider,
        "usage": { "include": true }
    });
    if let Some(schema) = response_schema {
        body["response_format"] = json!({
            "type": "json_schema",
            "json_schema": { "name": "output", "strict": true, "schema": schema }
        });
    }
    body
}

impl<T: HttpTransport> OpenRouter<T> {
    pub fn new(http: Arc<T>, upstream: Upstream) -> Self {
        Self { http, upstream }
    }

    pub fn upstream(&self) -> &Upstream {
        &self.upstream
    }

    async fn get(&self, req: HttpRequest) -> Result<Value, OrError> {
        let resp = self.http.send(req).await?;
        let body = resp.json().unwrap_or(Value::Null);
        if !resp.is_success() {
            return Err(OrError::Status { status: resp.status, body: body.to_string() });
        }
        Ok(body)
    }

    /// `GET /key` for the key actually used. Through AI Gateway this relies on provider
    /// passthrough of `/key` with the stored key; if the gateway doesn't proxy it, this fails
    /// and `key_check` policy decides what happens (docs/spend-control.md).
    pub async fn key_info(&self) -> Result<KeyInfo, OrError> {
        let req = self.upstream.authorize(HttpRequest::get(format!("{}/key", self.upstream.base())), None);
        let v = self.get(req).await?;
        serde_json::from_value(v.get("data").cloned().unwrap_or(Value::Null))
            .map_err(|e| OrError::Shape(format!("/key: {e}")))
    }

    /// Price table for all models (USD per token).
    pub async fn prices(&self) -> Result<HashMap<String, Price>, OrError> {
        // Public endpoint: never send a credential for it.
        let v = self.get(HttpRequest::get(PUBLIC_MODELS_URL)).await?;
        let data = v.get("data").and_then(Value::as_array).ok_or_else(|| OrError::Shape("/models".into()))?;
        let parse = |p: Option<&Value>| p.and_then(Value::as_str).and_then(|s| s.parse::<f64>().ok());
        Ok(data
            .iter()
            .filter_map(|m| {
                let id = m.get("id")?.as_str()?.to_string();
                let pricing = m.get("pricing")?;
                Some((
                    id,
                    Price { prompt: parse(pricing.get("prompt"))?, completion: parse(pricing.get("completion"))? },
                ))
            })
            .collect())
    }

    /// `metadata` is sent as `cf-aig-metadata` in AI Gateway mode (e.g. `{task, plugin}`).
    pub async fn complete(&self, model: &str, body: &Value, metadata: Option<&Value>) -> Result<Completion, OrError> {
        let req = HttpRequest::post_json(format!("{}/chat/completions", self.upstream.base()), body)?;
        let req = self.upstream.authorize(req, metadata);
        let resp = self.http.send(req).await?;
        let v = resp.json().unwrap_or(Value::Null);
        if !resp.is_success() {
            return Err(OrError::Status { status: resp.status, body: v.to_string() });
        }
        let content = v
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .ok_or_else(|| OrError::Shape("missing choices[0].message.content".into()))?
            .to_string();
        let usage = v.get("usage");
        Ok(Completion {
            content,
            model: v.get("model").and_then(Value::as_str).unwrap_or(model).to_string(),
            cost_usd: usage.and_then(|u| u.get("cost")).and_then(Value::as_f64),
            prompt_tokens: usage.and_then(|u| u.get("prompt_tokens")).and_then(Value::as_u64),
            completion_tokens: usage.and_then(|u| u.get("completion_tokens")).and_then(Value::as_u64),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routing_is_fixed_and_extras_only_tighten() {
        let mut extra = serde_json::Map::new();
        extra.insert("data_collection".into(), json!("allow")); // attempt to loosen
        extra.insert("zdr".into(), json!(true));
        extra.insert("allow_fallbacks".into(), json!(true)); // attempt to loosen
        let b = completion_body("a/m", &json!([]), Some(&json!({"type":"object"})), 100, (1.0, 4.0), &extra, &[]);
        let p = &b["provider"];
        assert_eq!(p["data_collection"], "deny");
        assert_eq!(p["sort"], "price");
        assert_eq!(p["zdr"], true);
        assert!(p.get("allow_fallbacks").is_none());
        assert_eq!(p["max_price"]["completion"], 4.0);
        assert_eq!(b["response_format"]["json_schema"]["strict"], true);
    }

    fn aig() -> Upstream {
        Upstream::AiGateway {
            base: "https://gateway.ai.cloudflare.com/v1/acct/cairn/openrouter/v1".into(),
            token: "aig-token".into(),
            alias: Some("cairn".into()),
        }
    }

    #[tokio::test]
    async fn ai_gateway_mode_sends_no_provider_key() {
        use cairn_http::FakeTransport;
        let t = Arc::new(FakeTransport::with_responses(vec![FakeTransport::json(
            200,
            json!({ "choices": [{ "message": { "content": "ok" } }] }),
        )]));
        let or = OpenRouter::new(t.clone(), aig());
        or.complete("a/m", &json!({}), Some(&json!({ "task": "nightly" }))).await.unwrap();
        let req = &t.sent()[0];
        let h = |k: &str| req.headers.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
        assert!(req.url.ends_with("/openrouter/v1/chat/completions"));
        assert_eq!(h("cf-aig-authorization").as_deref(), Some("Bearer aig-token"));
        assert_eq!(h("cf-aig-byok-alias").as_deref(), Some("cairn"));
        assert_eq!(h("cf-aig-no-wholesale").as_deref(), Some("true"), "never fall back to Unified Billing");
        assert!(h("cf-aig-metadata").unwrap().contains("nightly"));
        assert!(h("authorization").is_none(), "provider Authorization must be absent for BYOK injection");
    }

    #[tokio::test]
    async fn prices_use_public_endpoint_without_credentials() {
        use cairn_http::FakeTransport;
        let t = Arc::new(FakeTransport::with_responses(vec![FakeTransport::json(
            200,
            json!({ "data": [{ "id": "a/m", "pricing": { "prompt": "0.000001", "completion": "0.000004" } }] }),
        )]));
        let or = OpenRouter::new(t.clone(), aig());
        let p = or.prices().await.unwrap();
        assert_eq!(p["a/m"].completion, 0.000004);
        let req = &t.sent()[0];
        assert_eq!(req.url, PUBLIC_MODELS_URL);
        assert!(req.headers.is_empty(), "no credential on the public price list");
    }
}
