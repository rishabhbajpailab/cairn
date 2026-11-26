//! Model ordering and worst-case cost estimation.
//!
//! The caller (a task) supplies its own model list. The gateway only ever *removes* models
//! (not allowlisted) and re-orders them (`cheapest`). It never adds one.

use crate::config::GatewayConfig;
use crate::openrouter::Price;
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum Select {
    Cheapest,
    #[default]
    FirstAvailable,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    pub model: String,
    /// USD per million tokens. Worst-case prices come from the allowlist ceiling, never the live price.
    pub max_prompt_per_m: f64,
    pub max_completion_per_m: f64,
    /// Live price (USD per million), if known; used only for ordering.
    pub live: Option<(f64, f64)>,
}

pub fn order_models(
    cfg: &GatewayConfig,
    prices: &HashMap<String, Price>,
    requested: &[String],
    select: Select,
    io_ratio: f64,
) -> Vec<Candidate> {
    let mut out: Vec<Candidate> = requested
        .iter()
        .filter_map(|m| {
            let allow = cfg.allowed(m)?;
            let live = prices.get(m).map(|p| (p.prompt * 1e6, p.completion * 1e6));
            // A live price above the ceiling means OpenRouter would refuse via max_price anyway.
            if let Some((lp, lc)) = live
                && (lp > allow.max_prompt_usd_per_m || lc > allow.max_completion_usd_per_m)
            {
                tracing::warn!(model = %m, "live price above allowlist ceiling; skipping");
                return None;
            }
            Some(Candidate {
                model: m.clone(),
                max_prompt_per_m: allow.max_prompt_usd_per_m,
                max_completion_per_m: allow.max_completion_usd_per_m,
                live,
            })
        })
        .collect();

    if select == Select::Cheapest {
        let weight = |c: &Candidate| {
            let (p, q) = c.live.unwrap_or((c.max_prompt_per_m, c.max_completion_per_m));
            p * io_ratio + q
        };
        out.sort_by(|a, b| weight(a).partial_cmp(&weight(b)).unwrap_or(std::cmp::Ordering::Equal));
    }
    out
}

/// Conservative token estimate: ~3 chars per token over the serialized messages, plus overhead.
pub fn estimate_prompt_tokens(messages: &Value) -> u64 {
    let chars = messages.to_string().chars().count() as u64;
    chars / 3 + 64
}

pub fn worst_case_usd(c: &Candidate, prompt_tokens: u64, max_tokens: u32) -> f64 {
    (prompt_tokens as f64 * c.max_prompt_per_m + max_tokens as f64 * c.max_completion_per_m) / 1e6
}

/// Fallback when OpenRouter doesn't report `usage.cost`: price tokens at the ceiling.
pub fn cost_from_tokens(c: &Candidate, prompt: Option<u64>, completion: Option<u64>) -> Option<f64> {
    Some((prompt? as f64 * c.max_prompt_per_m + completion? as f64 * c.max_completion_per_m) / 1e6)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> GatewayConfig {
        GatewayConfig::parse(
            r#"
            [global]
            per_day_usd = 1
            per_month_usd = 10
            per_request_max_usd = 0.1
            [[allow]]
            model = "a/pricey"
            max_prompt_usd_per_m = 3
            max_completion_usd_per_m = 15
            [[allow]]
            model = "b/cheap"
            max_prompt_usd_per_m = 0.5
            max_completion_usd_per_m = 2
        "#,
        )
        .unwrap()
    }

    #[test]
    fn never_adds_models_and_drops_unlisted() {
        let req = vec!["a/pricey".to_string(), "z/unlisted".to_string()];
        let out = order_models(&cfg(), &HashMap::new(), &req, Select::Cheapest, 4.0);
        assert_eq!(out.iter().map(|c| c.model.as_str()).collect::<Vec<_>>(), vec!["a/pricey"]);
    }

    #[test]
    fn cheapest_reorders_within_list() {
        let req = vec!["a/pricey".to_string(), "b/cheap".to_string()];
        let out = order_models(&cfg(), &HashMap::new(), &req, Select::Cheapest, 4.0);
        assert_eq!(out[0].model, "b/cheap");
        let out = order_models(&cfg(), &HashMap::new(), &req, Select::FirstAvailable, 4.0);
        assert_eq!(out[0].model, "a/pricey");
    }

    #[test]
    fn worst_case_uses_ceiling() {
        let c = &order_models(&cfg(), &HashMap::new(), &["b/cheap".into()], Select::FirstAvailable, 1.0)[0];
        // 1000 prompt @ $0.5/M + 1000 completion @ $2/M = $0.0025
        assert!((worst_case_usd(c, 1000, 1000) - 0.0025).abs() < 1e-12);
    }
}
