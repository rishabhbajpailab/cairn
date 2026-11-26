use crate::id::Id;
use crate::query::Query;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ViewKind {
    Canvas,
    Board,
    Table,
    Calendar,
}

/// Per-view placement. Layout lives on views, never on items (invariant 3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LayoutEntry {
    pub x: f64,
    pub y: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub w: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub h: Option<f64>,
    #[serde(default)]
    pub collapsed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct View {
    pub id: Id,
    pub kind: ViewKind,
    pub name: String,
    #[serde(default)]
    pub query: Query,
    #[serde(default)]
    pub layout: BTreeMap<Id, LayoutEntry>,
    #[serde(default)]
    pub config: Map<String, Value>,
}
