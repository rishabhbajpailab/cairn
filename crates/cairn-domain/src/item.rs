use crate::capture::CaptureSource;
use crate::id::Id;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Lifecycle {
    #[default]
    Active,
    Archived,
    Trashed,
}

/// An item in the graph. `payload` is owned by the type plugin; the core never interprets it.
/// `idx` holds plugin-declared index fields copied out of `payload` (see cairn-plugins).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub id: Id,
    #[serde(rename = "type")]
    pub item_type: String,
    pub type_version: u32,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub payload: Map<String, Value>,
    #[serde(default)]
    pub idx: Map<String, Value>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub lifecycle: Lifecycle,
    #[serde(default)]
    pub pinned: bool,
    pub source: CaptureSource,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trashed_at: Option<DateTime<Utc>>,
}

impl Item {
    pub fn new(item_type: impl Into<String>, type_version: u32, source: CaptureSource) -> Self {
        let now = Utc::now();
        Self {
            id: Id::new(),
            item_type: item_type.into(),
            type_version,
            title: String::new(),
            payload: Map::new(),
            idx: Map::new(),
            tags: Vec::new(),
            lifecycle: Lifecycle::Active,
            pinned: false,
            source,
            created_at: now,
            updated_at: now,
            archived_at: None,
            trashed_at: None,
        }
    }

    /// Pinned items and items tagged `keep` are exempt from every automatic lifecycle rule.
    pub fn is_protected(&self) -> bool {
        self.pinned || self.tags.iter().any(|t| t == "keep")
    }
}
