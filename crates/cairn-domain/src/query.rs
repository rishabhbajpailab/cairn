use crate::id::Id;
use crate::item::Lifecycle;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Shared query language for views, smart buckets, task inputs and the API.
/// See docs/data-model.md § Query. `where`/`sort` fields must be core fields or plugin `idx` fields.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Query {
    pub types: Vec<String>,
    pub tags_all: Vec<String>,
    pub tags_any: Vec<String>,
    pub tags_none: Vec<String>,
    /// Empty = `[active]`.
    pub lifecycle: Vec<Lifecycle>,
    #[serde(rename = "where")]
    pub filters: Vec<Where>,
    pub text: Option<String>,
    pub related_to: Option<RelatedTo>,
    /// Relative ("-24h") or RFC 3339.
    pub updated_since: Option<String>,
    pub sort: Vec<SortKey>,
    pub limit: Option<u32>,
}

impl Query {
    pub fn effective_lifecycle(&self) -> Vec<Lifecycle> {
        if self.lifecycle.is_empty() { vec![Lifecycle::Active] } else { self.lifecycle.clone() }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Where {
    pub field: String,
    pub op: Cmp,
    pub value: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Cmp {
    Eq,
    Ne,
    Lt,
    Lte,
    Gt,
    Gte,
    In,
    Contains,
    Exists,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelatedTo {
    pub item: Id,
    pub kind: Option<String>,
    #[serde(default = "one")]
    pub depth: u8,
}

fn one() -> u8 {
    1
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SortKey {
    pub field: String,
    #[serde(default)]
    pub dir: SortDir,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SortDir {
    #[default]
    Asc,
    Desc,
}
