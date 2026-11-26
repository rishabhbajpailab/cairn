use crate::id::Id;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceKind {
    Rest,
    Webhook,
    Discord,
    Ui,
    Task,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaptureSource {
    pub kind: SourceKind,
    /// e.g. webhook source name, Discord message link, task-run id
    #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl CaptureSource {
    pub fn new(kind: SourceKind) -> Self {
        Self { kind, reference: None, label: None }
    }
}

/// Raw inbound payload. Stored before any parsing/validation (invariant 1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Capture {
    pub id: Id,
    pub source: CaptureSource,
    pub content_type: String,
    pub body: Value,
    pub received_at: DateTime<Utc>,
    #[serde(default)]
    pub resolved: bool,
    #[serde(default)]
    pub item: Option<Id>,
    #[serde(default)]
    pub error: Option<String>,
}

impl Capture {
    pub fn new(source: CaptureSource, content_type: impl Into<String>, body: Value) -> Self {
        Self {
            id: Id::new(),
            source,
            content_type: content_type.into(),
            body,
            received_at: Utc::now(),
            resolved: false,
            item: None,
            error: None,
        }
    }

    /// Best-effort text extraction: `{"text": "..."}` or a bare JSON string.
    pub fn text(&self) -> Option<&str> {
        match &self.body {
            Value::String(s) => Some(s),
            Value::Object(o) => o.get("text").and_then(Value::as_str),
            _ => None,
        }
    }
}
