use crate::{Applied, Result, Store, StoreError, check_ops};
use async_trait::async_trait;
use cairn_domain::{Capture, Cmp, Edge, Id, Item, Lifecycle, Op, OpKind, Query, SortDir, View, reltime};
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

/// In-memory store for tests. Implements the same semantics as `SurrealStore`
/// (atomic batches, ops log, GC-only purge, purge cascade).
#[derive(Default)]
pub struct MemStore {
    inner: RwLock<Inner>,
}

#[derive(Default, Clone)]
struct Inner {
    items: HashMap<Id, Item>,
    edges: HashMap<Id, Edge>,
    views: HashMap<Id, View>,
    captures: HashMap<Id, Capture>,
    ops: Vec<(Id, Op)>,
}

impl MemStore {
    pub fn new() -> Self {
        Self::default()
    }

    // std RwLock: never held across an await, and compiles everywhere (incl. wasm32).
    fn read(&self) -> RwLockReadGuard<'_, Inner> {
        self.inner.read().unwrap_or_else(|p| p.into_inner())
    }

    fn write(&self) -> RwLockWriteGuard<'_, Inner> {
        self.inner.write().unwrap_or_else(|p| p.into_inner())
    }

    /// Number of ops logged so far (test helper).
    pub fn op_count(&self) -> usize {
        self.read().ops.len()
    }

    pub fn capture(&self, id: Id) -> Option<Capture> {
        self.read().captures.get(&id).cloned()
    }

    /// All items regardless of lifecycle (test helper).
    pub fn all_items(&self) -> Vec<Item> {
        self.read().items.values().cloned().collect()
    }
}

fn apply_one(state: &mut Inner, op: &Op) -> Result<()> {
    match &op.kind {
        OpKind::ItemCreate { item } => {
            if state.items.contains_key(&item.id) {
                return Err(StoreError::Conflict(format!("item {} exists", item.id)));
            }
            state.items.insert(item.id, item.clone());
        }
        OpKind::ItemPatch { id, title, payload, tags, pinned, idx } => {
            let it = state.items.get_mut(id).ok_or_else(|| StoreError::NotFound(id.to_string()))?;
            if let Some(t) = title {
                it.title = t.clone();
            }
            if let Some(p) = payload {
                for (k, v) in p {
                    it.payload.insert(k.clone(), v.clone());
                }
            }
            if let Some(t) = tags {
                it.tags = t.clone();
            }
            if let Some(p) = pinned {
                it.pinned = *p;
            }
            if let Some(i) = idx {
                for (k, v) in i {
                    it.idx.insert(k.clone(), v.clone());
                }
            }
            it.updated_at = Utc::now();
        }
        OpKind::ItemLifecycle { id, to } => {
            let it = state.items.get_mut(id).ok_or_else(|| StoreError::NotFound(id.to_string()))?;
            let now = Utc::now();
            it.lifecycle = *to;
            match to {
                Lifecycle::Active => {
                    it.archived_at = None;
                    it.trashed_at = None;
                }
                Lifecycle::Archived => it.archived_at = Some(now),
                Lifecycle::Trashed => it.trashed_at = Some(now),
            }
            it.updated_at = now;
        }
        OpKind::ItemPurge { id } => {
            state.items.remove(id).ok_or_else(|| StoreError::NotFound(id.to_string()))?;
            state.edges.retain(|_, e| e.from != *id && e.to != *id);
            for v in state.views.values_mut() {
                v.layout.remove(id);
            }
        }
        OpKind::EdgeCreate { edge } => {
            for end in [edge.from, edge.to] {
                if !state.items.contains_key(&end) {
                    return Err(StoreError::NotFound(format!("edge endpoint {end}")));
                }
            }
            let dup = state.edges.values().any(|e| e.from == edge.from && e.to == edge.to && e.kind == edge.kind);
            if dup {
                return Err(StoreError::Conflict("duplicate edge".into()));
            }
            state.edges.insert(edge.id, edge.clone());
        }
        OpKind::EdgeDelete { id } => {
            state.edges.remove(id).ok_or_else(|| StoreError::NotFound(id.to_string()))?;
        }
        OpKind::ViewCreate { view } => {
            state.views.insert(view.id, view.clone());
        }
        OpKind::ViewLayout { id, item, entry } => {
            let v = state.views.get_mut(id).ok_or_else(|| StoreError::NotFound(id.to_string()))?;
            match entry {
                Some(e) => {
                    v.layout.insert(*item, e.clone());
                }
                None => {
                    v.layout.remove(item);
                }
            }
        }
    }
    Ok(())
}

#[async_trait]
impl Store for MemStore {
    async fn apply(&self, ops: Vec<Op>) -> Result<Applied> {
        check_ops(&ops)?;
        let mut guard = self.write();
        // Apply to a scratch copy so a failing op leaves state untouched (atomic batch).
        let mut scratch = guard.clone();
        let batch = Id::new();
        for op in &ops {
            apply_one(&mut scratch, op)?;
            scratch.ops.push((batch, op.clone()));
        }
        *guard = scratch;
        Ok(Applied { batch, count: ops.len() })
    }

    async fn get_item(&self, id: Id) -> Result<Option<Item>> {
        Ok(self.read().items.get(&id).cloned())
    }

    async fn query_items(&self, q: &Query, now: DateTime<Utc>) -> Result<Vec<Item>> {
        let since = match &q.updated_since {
            Some(s) => Some(reltime::resolve(s, now).map_err(|e| StoreError::InvalidQuery(e.to_string()))?),
            None => None,
        };
        let lifecycles = q.effective_lifecycle();
        let text = q.text.as_ref().map(|t| t.to_lowercase());
        let state = self.read();

        let mut out: Vec<Item> = state
            .items
            .values()
            .filter(|it| lifecycles.contains(&it.lifecycle))
            .filter(|it| q.types.is_empty() || q.types.contains(&it.item_type))
            .filter(|it| q.tags_all.iter().all(|t| it.tags.contains(t)))
            .filter(|it| q.tags_any.is_empty() || q.tags_any.iter().any(|t| it.tags.contains(t)))
            .filter(|it| !q.tags_none.iter().any(|t| it.tags.contains(t)))
            .filter(|it| since.is_none_or(|s| it.updated_at >= s))
            .filter(|it| text.as_ref().is_none_or(|t| it.title.to_lowercase().contains(t)))
            .filter(|it| {
                q.filters.iter().all(|w| matches_where(it, &w.field, w.op, &reltime::resolve_json(&w.value, now)))
            })
            .cloned()
            .collect();

        if let Some(rel) = &q.related_to {
            // depth-1 only in MemStore; SurrealStore handles deeper traversal.
            let neighbours: Vec<Id> = state
                .edges
                .values()
                .filter(|e| rel.kind.as_ref().is_none_or(|k| edge_kind_str(e) == *k))
                .filter_map(|e| {
                    if e.from == rel.item {
                        Some(e.to)
                    } else if e.to == rel.item {
                        Some(e.from)
                    } else {
                        None
                    }
                })
                .collect();
            out.retain(|it| neighbours.contains(&it.id));
        }

        // Default order: newest first. Explicit sort keys override.
        out.sort_by_key(|it| std::cmp::Reverse(it.updated_at));
        for key in q.sort.iter().rev() {
            out.sort_by(|a, b| {
                let ord = cmp_values(&field(a, &key.field), &field(b, &key.field));
                if key.dir == SortDir::Desc { ord.reverse() } else { ord }
            });
        }
        if let Some(limit) = q.limit {
            out.truncate(limit as usize);
        }
        Ok(out)
    }

    async fn edges_of(&self, id: Id) -> Result<Vec<Edge>> {
        let state = self.read();
        Ok(state.edges.values().filter(|e| e.from == id || e.to == id).cloned().collect())
    }

    async fn get_view(&self, id: Id) -> Result<Option<View>> {
        Ok(self.read().views.get(&id).cloned())
    }

    async fn put_capture(&self, capture: Capture) -> Result<()> {
        self.write().captures.insert(capture.id, capture);
        Ok(())
    }

    async fn resolve_capture(&self, id: Id, item: Option<Id>, error: Option<String>) -> Result<()> {
        let mut state = self.write();
        let c = state.captures.get_mut(&id).ok_or_else(|| StoreError::NotFound(id.to_string()))?;
        c.resolved = item.is_some();
        c.item = item;
        c.error = error;
        Ok(())
    }

    async fn purge_resolved_captures(&self, before: DateTime<Utc>) -> Result<usize> {
        let mut state = self.write();
        let n = state.captures.len();
        state.captures.retain(|_, c| !(c.resolved && c.received_at < before));
        Ok(n - state.captures.len())
    }
}

fn edge_kind_str(e: &Edge) -> String {
    serde_json::to_value(&e.kind).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default()
}

/// Resolve a field reference: core fields by name, `idx.<field>` or bare plugin index fields.
fn field(it: &Item, name: &str) -> Value {
    match name {
        "title" => Value::String(it.title.clone()),
        "type" => Value::String(it.item_type.clone()),
        "created_at" => Value::String(it.created_at.to_rfc3339()),
        "updated_at" => Value::String(it.updated_at.to_rfc3339()),
        "pinned" => Value::Bool(it.pinned),
        other => {
            let key = other.strip_prefix("idx.").unwrap_or(other);
            it.idx.get(key).cloned().unwrap_or(Value::Null)
        }
    }
}

fn matches_where(it: &Item, name: &str, op: Cmp, value: &Value) -> bool {
    let v = field(it, name);
    use std::cmp::Ordering::*;
    match op {
        Cmp::Eq => v == *value,
        Cmp::Ne => v != *value,
        Cmp::Lt => cmp_values(&v, value) == Less,
        Cmp::Lte => cmp_values(&v, value) != Greater,
        Cmp::Gt => cmp_values(&v, value) == Greater,
        Cmp::Gte => cmp_values(&v, value) != Less,
        Cmp::In => value.as_array().is_some_and(|a| a.contains(&v)),
        Cmp::Contains => {
            v.as_array().is_some_and(|a| a.contains(value))
                || v.as_str().zip(value.as_str()).is_some_and(|(h, n)| h.contains(n))
        }
        Cmp::Exists => !v.is_null(),
    }
}

fn cmp_values(a: &Value, b: &Value) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64().partial_cmp(&y.as_f64()).unwrap_or(Ordering::Equal),
        (Value::String(x), Value::String(y)) => x.cmp(y),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        (Value::Null, Value::Null) => Ordering::Equal,
        (Value::Null, _) => Ordering::Greater, // nulls last
        (_, Value::Null) => Ordering::Less,
        _ => Ordering::Equal,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cairn_domain::{Actor, CaptureSource, EdgeKind, SourceKind};

    fn item(t: &str, title: &str) -> Item {
        let mut it = Item::new(t, 1, CaptureSource::new(SourceKind::Rest));
        it.title = title.into();
        it
    }

    #[tokio::test]
    async fn batch_is_atomic() {
        let s = MemStore::new();
        let a = item("todo", "a");
        let ghost = Id::new();
        let res = s
            .apply(vec![
                Op::new(Actor::User, OpKind::ItemCreate { item: a.clone() }),
                Op::new(Actor::User, OpKind::EdgeCreate { edge: Edge::new(a.id, ghost, EdgeKind::Relates) }),
            ])
            .await;
        assert!(res.is_err());
        assert!(s.get_item(a.id).await.unwrap().is_none(), "first op must roll back");
        assert_eq!(s.op_count(), 0);
    }

    #[tokio::test]
    async fn only_gc_can_purge() {
        let s = MemStore::new();
        let a = item("todo", "a");
        s.apply(vec![Op::new(Actor::User, OpKind::ItemCreate { item: a.clone() })]).await.unwrap();
        let err = s.apply(vec![Op::new(Actor::User, OpKind::ItemPurge { id: a.id })]).await;
        assert!(matches!(err, Err(StoreError::Forbidden(_))));
        s.apply(vec![Op::new(Actor::Gc, OpKind::ItemPurge { id: a.id })]).await.unwrap();
        assert!(s.get_item(a.id).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn purge_cascades_edges() {
        let s = MemStore::new();
        let (a, b) = (item("bucket", "home"), item("todo", "dmv"));
        s.apply(vec![
            Op::new(Actor::User, OpKind::ItemCreate { item: a.clone() }),
            Op::new(Actor::User, OpKind::ItemCreate { item: b.clone() }),
            Op::new(Actor::User, OpKind::EdgeCreate { edge: Edge::new(a.id, b.id, EdgeKind::Contains) }),
        ])
        .await
        .unwrap();
        s.apply(vec![Op::new(Actor::Gc, OpKind::ItemPurge { id: b.id })]).await.unwrap();
        assert!(s.edges_of(a.id).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn query_filters_and_default_lifecycle() {
        let s = MemStore::new();
        let mut t = item("todo", "Renew registration");
        t.tags = vec!["car".into()];
        let archived = item("todo", "old");
        s.apply(vec![
            Op::new(Actor::User, OpKind::ItemCreate { item: t.clone() }),
            Op::new(Actor::User, OpKind::ItemCreate { item: archived.clone() }),
            Op::new(Actor::User, OpKind::ItemLifecycle { id: archived.id, to: Lifecycle::Archived }),
        ])
        .await
        .unwrap();
        let q = Query { types: vec!["todo".into()], ..Default::default() };
        let got = s.query_items(&q, Utc::now()).await.unwrap();
        assert_eq!(got.len(), 1, "archived items hidden by default");
        let q = Query { text: Some("registration".into()), tags_all: vec!["car".into()], ..Default::default() };
        assert_eq!(s.query_items(&q, Utc::now()).await.unwrap()[0].id, t.id);
    }
}
