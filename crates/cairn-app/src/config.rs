use serde::Deserialize;

/// `config/cairn.toml`: non-secret settings, committed and embedded into the API Worker at
/// build time. Secrets are Worker secrets (`wrangler secret put`), never in this file.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoreConfig {
    #[serde(default = "default_tz")]
    pub timezone: String,
    #[serde(default)]
    pub surreal: Option<SurrealSection>,
    #[serde(default)]
    pub discord: DiscordSection,
    #[serde(default)]
    pub notify: NotifySection,
    #[serde(default)]
    pub gc: Option<cairn_gc::GcConfig>,
    #[serde(default, rename = "webhook")]
    pub webhooks: Vec<WebhookSource>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SurrealSection {
    /// HTTPS endpoint of the Surreal Cloud instance.
    pub url: String,
    pub namespace: String,
    pub database: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiscordSection {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub dm_user_id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NotifySection {
    #[serde(default = "default_max_reminders")]
    pub max_reminders_per_day: u32,
    /// Local times "HH:MM": reminders due inside this window wait until it ends.
    #[serde(default)]
    pub quiet_hours: Option<[String; 2]>,
}

impl Default for NotifySection {
    fn default() -> Self {
        Self { max_reminders_per_day: default_max_reminders(), quiet_hours: None }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebhookSource {
    pub source: String,
    pub token_sha256: String,
    #[serde(default)]
    pub map: toml::Table,
}

fn default_tz() -> String {
    "UTC".into()
}
fn default_max_reminders() -> u32 {
    8
}

impl CoreConfig {
    pub fn parse(raw: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(raw)
    }

    pub fn tz(&self) -> chrono_tz::Tz {
        self.timezone.parse().unwrap_or(chrono_tz::UTC)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_config_parses() {
        let cfg =
            CoreConfig::parse(include_str!("../../../config/cairn.toml")).expect("config/cairn.toml must stay valid");
        assert_eq!(cfg.tz(), chrono_tz::America::Los_Angeles);
        assert!(cfg.gc.is_some());
    }
}
