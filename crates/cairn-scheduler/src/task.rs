use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Typed view of a `task` item payload. JSON Schema in plugins/types/task/schema.json is
/// authoritative; this struct must stay in sync with it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskDef {
    pub name: String,
    /// `ai/<capability>` or `builtin/<name>`
    pub uses: String,
    #[serde(default = "yes")]
    pub enabled: bool,
    pub trigger: Trigger,
    #[serde(default)]
    pub input: Input,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub select: Select,
    #[serde(default)]
    pub provider: Map<String, Value>,
    #[serde(default)]
    pub fanout: Option<Fanout>,
    #[serde(default)]
    pub budget: Option<Budget>,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u32,
    #[serde(default)]
    pub apply: Apply,
    #[serde(default = "default_retain")]
    pub retain_runs: String,
}

fn yes() -> bool {
    true
}
fn default_max_tokens() -> u32 {
    1500
}
fn default_retain() -> String {
    "30d".into()
}

impl TaskDef {
    pub fn is_ai(&self) -> bool {
        self.uses.starts_with("ai/")
    }

    pub fn capability(&self) -> &str {
        self.uses.split_once('/').map(|(_, n)| n).unwrap_or(&self.uses)
    }

    /// Upper bound on gateway requests per run (fan-out chunks, or 1).
    pub fn max_requests_per_run(&self) -> u32 {
        self.fanout.as_ref().map(|f| f.max).unwrap_or(1)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    Cron(String),
    On {
        event: String,
        #[serde(default)]
        r#where: Map<String, Value>,
    },
    After(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Input {
    #[serde(default)]
    pub query: cairn_domain::Query,
    #[serde(default = "default_max_items")]
    pub max_items: u32,
}

fn default_max_items() -> u32 {
    200
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Select {
    Cheapest,
    #[default]
    FirstAvailable,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Fanout {
    pub by: String,
    #[serde(default)]
    pub chunk_size: Option<u32>,
    pub max: u32,
    #[serde(default = "two")]
    pub concurrency: u32,
}

fn two() -> u32 {
    2
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Budget {
    pub per_run_usd: f64,
    pub per_day_usd: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Apply {
    Auto,
    #[default]
    Review,
}
