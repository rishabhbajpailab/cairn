//! `config/gateway.toml`: compiled into the gateway Worker (`include_str!`). There is no API or
//! binding that can change it at runtime: raising a cap means editing the file and running
//! `wrangler deploy` with credentials the app never holds (docs/spend-control.md).

use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GatewayConfig {
    pub global: Global,
    #[serde(default)]
    pub breaker: Breaker,
    #[serde(default)]
    pub reconcile: Reconcile,
    /// Model allowlist. Anything not listed is rejected.
    #[serde(default)]
    pub allow: Vec<AllowedModel>,
    /// Per AI capability daily caps.
    #[serde(default)]
    pub plugins: BTreeMap<String, PluginCap>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Global {
    /// What to do when the hourly `GET /key` check can't reach OpenRouter's key info
    /// (e.g. AI Gateway doesn't proxy `/key`). A *positive* finding (management key, no
    /// credit limit) always disables, in either mode.
    #[serde(default)]
    pub key_check: KeyCheck,
    pub per_day_usd: f64,
    pub per_month_usd: f64,
    pub per_request_max_usd: f64,
    /// Account-wide provider allowlist mirror (also set in OpenRouter privacy settings).
    #[serde(default)]
    pub providers_only: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum KeyCheck {
    /// Refuse completions until the key has been verified capped (fail closed).
    #[default]
    Required,
    /// Proceed if the check is unreachable; rely on the AI Gateway spend limit and the
    /// OpenRouter key limit (both set in dashboards). Use only after confirming them.
    BestEffort,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Breaker {
    pub max_usd_per_hour: f64,
    pub max_rejections_per_10m: u32,
}

impl Default for Breaker {
    fn default() -> Self {
        Self { max_usd_per_hour: 0.25, max_rejections_per_10m: 20 }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reconcile {
    pub tolerance_usd: f64,
}

impl Default for Reconcile {
    fn default() -> Self {
        Self { tolerance_usd: 0.05 }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AllowedModel {
    pub model: String,
    /// USD per million tokens; sent as `provider.max_price` and used as the worst-case price.
    pub max_prompt_usd_per_m: f64,
    pub max_completion_usd_per_m: f64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginCap {
    pub per_day_usd: f64,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("parsing gateway config: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("invalid gateway config: {0}")]
    Invalid(String),
}

impl GatewayConfig {
    pub fn parse(raw: &str) -> Result<Self, ConfigError> {
        let cfg: GatewayConfig = toml::from_str(raw)?;
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        let g = &self.global;
        let positive = [g.per_day_usd, g.per_month_usd, g.per_request_max_usd];
        if positive.iter().any(|v| !v.is_finite() || *v <= 0.0) {
            return Err(ConfigError::Invalid("global caps must be finite and > 0".into()));
        }
        if g.per_request_max_usd > g.per_day_usd || g.per_day_usd > g.per_month_usd {
            return Err(ConfigError::Invalid("expected per_request_max <= per_day <= per_month".into()));
        }
        if self.allow.is_empty() {
            return Err(ConfigError::Invalid("model allowlist is empty".into()));
        }
        for m in &self.allow {
            if m.max_prompt_usd_per_m <= 0.0 || m.max_completion_usd_per_m <= 0.0 {
                return Err(ConfigError::Invalid(format!("{}: max prices must be > 0", m.model)));
            }
        }
        Ok(())
    }

    pub fn allowed(&self, model: &str) -> Option<&AllowedModel> {
        self.allow.iter().find(|m| m.model == model)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_parses() {
        let raw = include_str!("../../../config/gateway.toml");
        let cfg = GatewayConfig::parse(raw).expect("example config must stay valid");
        assert!(!cfg.allow.is_empty());
    }

    #[test]
    fn rejects_inverted_caps() {
        let raw = r#"
            [global]
            per_day_usd = 5.0
            per_month_usd = 1.0
            per_request_max_usd = 0.1
            [[allow]]
            model = "x/y"
            max_prompt_usd_per_m = 1
            max_completion_usd_per_m = 1
        "#;
        assert!(GatewayConfig::parse(raw).is_err());
    }
}
