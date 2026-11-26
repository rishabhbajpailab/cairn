use crate::edge::Edge;
use crate::id::Id;
use crate::item::{Item, Lifecycle};
use crate::view::{LayoutEntry, View};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Who performed a mutation. Serialized as `user`, `gc`, `task-run:<id>`, `ingest:<source>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "ref", rename_all = "kebab-case")]
pub enum Actor {
    User,
    Gc,
    TaskRun(Id),
    Ingest(String),
}

impl Actor {
    pub fn as_log_string(&self) -> String {
        match self {
            Actor::User => "user".into(),
            Actor::Gc => "gc".into(),
            Actor::TaskRun(id) => format!("task-run:{id}"),
            Actor::Ingest(src) => format!("ingest:{src}"),
        }
    }
}

/// A single mutation. Every write goes through `Store::apply(Vec<Op>)` (invariant 4),
/// which persists the change and appends it to the ops log in one transaction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OpKind {
    ItemCreate {
        item: Item,
    },
    /// Shallow merge into payload; `None` fields are left untouched.
    ItemPatch {
        id: Id,
        #[serde(default)]
        title: Option<String>,
        #[serde(default)]
        payload: Option<Map<String, Value>>,
        #[serde(default)]
        tags: Option<Vec<String>>,
        #[serde(default)]
        pinned: Option<bool>,
        /// Recomputed plugin index fields (set by the core whenever payload changes).
        #[serde(default)]
        idx: Option<Map<String, Value>>,
    },
    ItemLifecycle {
        id: Id,
        to: Lifecycle,
    },
    /// Hard delete. Only `Actor::Gc` may emit this (enforced by `Store::apply`).
    ItemPurge {
        id: Id,
    },
    EdgeCreate {
        edge: Edge,
    },
    EdgeDelete {
        id: Id,
    },
    ViewCreate {
        view: View,
    },
    ViewLayout {
        id: Id,
        item: Id,
        entry: Option<LayoutEntry>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Op {
    pub actor: Actor,
    #[serde(flatten)]
    pub kind: OpKind,
}

impl Op {
    pub fn new(actor: Actor, kind: OpKind) -> Self {
        Self { actor, kind }
    }

    /// Ops that only GC is allowed to emit.
    pub fn requires_gc(&self) -> bool {
        matches!(self.kind, OpKind::ItemPurge { .. })
    }
}
