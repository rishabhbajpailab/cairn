use crate::id::Id;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Core edge kinds. Plugins may declare additional kinds (stored as `Other`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EdgeKind {
    Contains,
    Relates,
    Blocks,
    DerivedFrom,
    ProducedBy,
    After,
    RemindAbout,
    Suggested,
    #[serde(untagged)]
    Other(String),
}

/// Directed edge `from -> to` (SurrealDB `in -> link -> out`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Edge {
    pub id: Id,
    pub from: Id,
    pub to: Id,
    pub kind: EdgeKind,
    #[serde(default)]
    pub props: Map<String, Value>,
    pub created_at: DateTime<Utc>,
}

impl Edge {
    pub fn new(from: Id, to: Id, kind: EdgeKind) -> Self {
        Self { id: Id::new(), from, to, kind, props: Map::new(), created_at: Utc::now() }
    }
}
