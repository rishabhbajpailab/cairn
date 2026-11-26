use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

/// `plugins/types/<name>/plugin.json`. Contract: docs/plugins.md.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TypeManifest {
    pub kind: ManifestKindType,
    pub name: String,
    pub version: u32,
    pub title: String,
    #[serde(default)]
    pub icon: Option<String>,
    pub schema: String,
    /// Payload fields exposed to where/sort/views, copied into `item.idx`.
    #[serde(default)]
    pub index: BTreeMap<String, IndexKind>,
    #[serde(default)]
    pub title_from: Option<String>,
    #[serde(default)]
    pub edge_kinds: Vec<String>,
    #[serde(default)]
    pub capture: Vec<CaptureMatcher>,
    #[serde(default)]
    pub actions: BTreeMap<String, ActionSpec>,
    #[serde(default)]
    pub views: Option<String>,
    #[serde(default)]
    pub migrations: Option<String>,
    /// Types the core relies on (capture, task, task-run, reminder, bucket) set this.
    #[serde(default)]
    pub core: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ManifestKindType {
    Type,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum IndexKind {
    String,
    Int,
    Float,
    Bool,
    Datetime,
}

/// Declarative only: plugins never run code during ingest.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CaptureMatcher {
    Prefix { prefix: String, field: String },
}

/// Declarative actions shared by UI, Discord buttons and AI-proposed ops.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionSpec {
    /// Shallow-merge into payload.
    #[serde(default)]
    pub patch: Option<Map<String, Value>>,
    /// Relative time, e.g. "+1d": sets payload `snoozed_until`.
    #[serde(default)]
    pub snooze: Option<String>,
    /// "archive" | "trash" | "active"
    #[serde(default)]
    pub lifecycle: Option<String>,
    /// If set, the notifier renders this action as a Discord button with this label.
    #[serde(default)]
    pub discord_button: Option<String>,
}

/// `plugins/ai/<name>/plugin.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AiManifest {
    pub kind: ManifestKindAi,
    pub name: String,
    pub version: u32,
    pub prompt: String,
    pub input: AiInput,
    pub output: String,
    /// Name of a Rust converter in cairn-app::ai_apply.
    pub apply: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ManifestKindAi {
    Ai,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiInput {
    /// "items" (list) or "item" (one at a time)
    pub shape: String,
    #[serde(default)]
    pub fields: Vec<String>,
    /// Typical input:output token ratio, used by `select: cheapest` cost weighting.
    #[serde(default)]
    pub io_ratio: Option<f64>,
}
