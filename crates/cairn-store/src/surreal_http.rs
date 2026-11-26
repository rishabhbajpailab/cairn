//! SurrealDB 3.x/4.x over HTTP (`POST /rpc`, JSON-RPC `query` method), ADR 0011.
//!
//! Why HTTP instead of the Rust SDK: Workers are short-lived and stateless, SurrealDB's own
//! guidance for serverless is HTTP, and the SDK's WebSocket engine is not a fit for
//! `wasm32-unknown-unknown` inside Workers. Each call is one stateless request carrying
//! namespace/database headers and credentials.
//!
//! Conventions:
//! - Record keys are ULIDs: `item:⟨01J…⟩`, built server-side with `type::record(tb, $key)`.
//! - Everything is bound as variables. Field *names* that reach SurrealQL text (where/sort)
//!   are validated against a strict pattern first.
//! - `apply` sends one request: BEGIN; op statements + op-log rows; COMMIT. A `THROW` anywhere
//!   aborts the whole batch.
//!
//! VERIFY IN M1 against a live instance: RPC envelope shape, header names (`Surreal-NS`,
//! `Surreal-DB`), and the SurrealQL in `schema/` (see docs/ROADMAP.md).

use crate::{Applied, Result, Store, StoreError, check_ops};
use async_trait::async_trait;
use base64::Engine;
use cairn_domain::{Capture, Cmp, Edge, Id, Item, Lifecycle, Op, OpKind, Query, SortDir, View, reltime};
use cairn_http::{HttpRequest, HttpTransport};
use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct SurrealConfig {
    /// HTTPS endpoint of the instance, e.g. `https://<instance>.aws-use1.surreal.cloud`.
    pub url: String,
    pub namespace: String,
    pub database: String,
    /// Database-level user (least privilege).
    pub username: String,
    pub password: String,
}

pub struct SurrealHttpStore<T: HttpTransport> {
    http: Arc<T>,
    cfg: SurrealConfig,
}

/// A schema file to apply, in order (the Workers glue embeds `schema/*.surql`).
pub struct SchemaFile<'a> {
    pub name: &'a str,
    pub sql: &'a str,
}

/// Generic message SurrealDB puts on every statement of an aborted transaction.
const NOT_EXECUTED: &str = "not executed due to a failed transaction";

fn db_err(msg: impl Into<String>) -> StoreError {
    let msg = msg.into();
    if msg.contains("not found:") {
        StoreError::NotFound(msg)
    } else if msg.contains("already exists") || msg.contains("already contains") {
        StoreError::Conflict(msg)
    } else {
        StoreError::Db(msg)
    }
}

impl<T: HttpTransport> SurrealHttpStore<T> {
    pub fn new(http: Arc<T>, cfg: SurrealConfig) -> Self {
        Self { http, cfg }
    }

    /// Run SurrealQL with bound variables; returns one result per statement.
    pub async fn rpc_query(&self, sql: &str, vars: Value) -> Result<Vec<Value>> {
        let body = json!({ "id": 1, "method": "query", "params": [sql, vars] });
        let auth =
            base64::engine::general_purpose::STANDARD.encode(format!("{}:{}", self.cfg.username, self.cfg.password));
        let req = HttpRequest::post_json(format!("{}/rpc", self.cfg.url.trim_end_matches('/')), &body)
            .map_err(|e| StoreError::Db(e.to_string()))?
            .header("accept", "application/json")
            .header("surreal-ns", &self.cfg.namespace)
            .header("surreal-db", &self.cfg.database)
            .header("authorization", format!("Basic {auth}"));
        let resp = self.http.send(req).await.map_err(|e| StoreError::Db(e.to_string()))?;
        let v = resp.json().map_err(|e| StoreError::Db(format!("HTTP {}: {e}: {}", resp.status, resp.text())))?;
        if let Some(err) = v.get("error") {
            let msg = err.get("message").and_then(Value::as_str).unwrap_or("rpc error");
            return Err(db_err(msg));
        }
        let results = v.get("result").and_then(Value::as_array).cloned().unwrap_or_default();
        // Surface the *real* failure, not the generic "not executed" on sibling statements.
        let mut first_err: Option<String> = None;
        for r in &results {
            if r.get("status").and_then(Value::as_str) == Some("ERR") {
                let msg = r.get("result").and_then(Value::as_str).unwrap_or("statement failed").to_string();
                if !msg.contains(NOT_EXECUTED) {
                    return Err(db_err(msg));
                }
                first_err.get_or_insert(msg);
            }
        }
        if let Some(msg) = first_err {
            return Err(db_err(msg));
        }
        Ok(results.into_iter().map(|r| r.get("result").cloned().unwrap_or(Value::Null)).collect())
    }

    async fn query_json<R: DeserializeOwned>(&self, sql: &str, vars: Value) -> Result<R> {
        let mut results = self.rpc_query(sql, vars).await?;
        let first = if results.is_empty() { Value::Null } else { results.swap_remove(0) };
        Ok(serde_json::from_value(first)?)
    }

    /// Apply schema files not yet recorded in `_migration`, in the order given.
    pub async fn migrate(&self, files: &[SchemaFile<'_>]) -> Result<Vec<String>> {
        let applied: Vec<String> =
            self.query_json("SELECT VALUE record::id(id) FROM _migration", json!({})).await.unwrap_or_default();
        let mut ran = Vec::new();
        for f in files {
            if applied.iter().any(|a| a == f.name) {
                continue;
            }
            let sql = format!("{}\nCREATE type::record('_migration', $name) RETURN NONE;", f.sql);
            self.rpc_query(&sql, json!({ "name": f.name })).await?;
            tracing::info!(migration = %f.name, "applied schema migration");
            ran.push(f.name.to_string());
        }
        Ok(ran)
    }
}

/// Field names allowed in where/sort: core fields or `idx.<snake_case>`.
fn safe_field(name: &str) -> Result<String> {
    const CORE: &[&str] = &["title", "type", "created_at", "updated_at", "pinned", "lifecycle", "type_version"];
    if CORE.contains(&name) {
        return Ok(name.to_string());
    }
    let key = name.strip_prefix("idx.").unwrap_or(name);
    let ok = !key.is_empty()
        && key.len() <= 64
        && key.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && key.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if ok { Ok(format!("idx.{key}")) } else { Err(StoreError::InvalidQuery(format!("field `{name}` not allowed"))) }
}

const ITEM_PROJECTION: &str = "*, record::id(id) AS id";
const EDGE_PROJECTION: &str =
    "record::id(id) AS id, record::id(in) AS from, record::id(out) AS to, kind, props, created_at";

/// Content for CREATE: drop fields the database owns (id, timestamps).
fn item_content(item: &Item) -> Result<Value> {
    let mut v = serde_json::to_value(item)?;
    if let Value::Object(o) = &mut v {
        for k in ["id", "created_at", "updated_at", "archived_at", "trashed_at"] {
            o.remove(k);
        }
    }
    Ok(v)
}

fn op_target(kind: &OpKind) -> String {
    match kind {
        OpKind::ItemCreate { item } => format!("item:{}", item.id),
        OpKind::ItemPatch { id, .. } | OpKind::ItemLifecycle { id, .. } | OpKind::ItemPurge { id } => {
            format!("item:{id}")
        }
        OpKind::EdgeCreate { edge } => format!("link:{}", edge.id),
        OpKind::EdgeDelete { id } => format!("link:{id}"),
        OpKind::ViewCreate { view } => format!("view:{}", view.id),
        OpKind::ViewLayout { id, .. } => format!("view:{id}"),
    }
}

fn op_kind_name(kind: &OpKind) -> &'static str {
    match kind {
        OpKind::ItemCreate { .. } => "item.create",
        OpKind::ItemPatch { .. } => "item.patch",
        OpKind::ItemLifecycle { .. } => "item.lifecycle",
        OpKind::ItemPurge { .. } => "item.purge",
        OpKind::EdgeCreate { .. } => "edge.create",
        OpKind::EdgeDelete { .. } => "edge.delete",
        OpKind::ViewCreate { .. } => "view.create",
        OpKind::ViewLayout { .. } => "view.patch",
    }
}

const EXISTS_ITEM: &str = "IF !record::exists(type::record('item', $__ID)) { THROW 'not found: item ' + $__ID; };";

/// Emit the SurrealQL for one op, binding its variables with the `p{n}_` prefix.
fn op_sql(n: usize, op: &Op, vars: &mut Map<String, Value>) -> Result<String> {
    let v = |name: &str| format!("$p{n}_{name}");
    let mut bind = |name: &str, val: Value| {
        vars.insert(format!("p{n}_{name}"), val);
    };
    let exists = |var: String| EXISTS_ITEM.replace("$__ID", &var);

    Ok(match &op.kind {
        OpKind::ItemCreate { item } => {
            bind("id", json!(item.id));
            bind("c", item_content(item)?);
            format!("CREATE type::record('item', {}) CONTENT {} RETURN NONE;", v("id"), v("c"))
        }
        OpKind::ItemPatch { id, title, payload, tags, pinned, idx } => {
            bind("id", json!(id));
            let mut m = Map::new();
            if let Some(t) = title {
                m.insert("title".into(), json!(t));
            }
            if let Some(p) = payload {
                m.insert("payload".into(), Value::Object(p.clone()));
            }
            if let Some(t) = tags {
                m.insert("tags".into(), json!(t));
            }
            if let Some(p) = pinned {
                m.insert("pinned".into(), json!(p));
            }
            if let Some(i) = idx {
                m.insert("idx".into(), Value::Object(i.clone()));
            }
            bind("m", Value::Object(m));
            format!("{} UPDATE type::record('item', {}) MERGE {} RETURN NONE;", exists(v("id")), v("id"), v("m"))
        }
        OpKind::ItemLifecycle { id, to } => {
            bind("id", json!(id));
            let set = match to {
                Lifecycle::Active => "lifecycle = 'active', archived_at = NONE, trashed_at = NONE",
                Lifecycle::Archived => "lifecycle = 'archived', archived_at = time::now()",
                Lifecycle::Trashed => "lifecycle = 'trashed', trashed_at = time::now()",
            };
            format!("{} UPDATE type::record('item', {}) SET {set} RETURN NONE;", exists(v("id")), v("id"))
        }
        OpKind::ItemPurge { id } => {
            bind("id", json!(id));
            bind("target", json!(format!("item:{id}")));
            let rec = format!("type::record('item', {})", v("id"));
            format!(
                "{exists}
                 DELETE link WHERE in = {rec} OR out = {rec};
                 UPDATE view SET layout = object::from_entries(object::entries(layout).filter(|$e| $e[0] != {id})) RETURN NONE;
                 UPDATE op SET before = NONE, after = NONE WHERE target = {target} RETURN NONE;
                 DELETE {rec};",
                exists = exists(v("id")),
                id = v("id"),
                target = v("target"),
            )
        }
        OpKind::EdgeCreate { edge } => {
            bind("id", json!(edge.id));
            bind("from", json!(edge.from));
            bind("to", json!(edge.to));
            bind("kind", serde_json::to_value(&edge.kind)?);
            bind("props", Value::Object(edge.props.clone()));
            format!(
                "{} {} INSERT RELATION INTO link {{ id: {id}, in: type::record('item', {from}), out: type::record('item', {to}), kind: {kind}, props: {props} }} RETURN NONE;",
                exists(v("from")),
                exists(v("to")),
                id = v("id"),
                from = v("from"),
                to = v("to"),
                kind = v("kind"),
                props = v("props"),
            )
        }
        OpKind::EdgeDelete { id } => {
            bind("id", json!(id));
            format!(
                "IF !record::exists(type::record('link', {id})) {{ THROW 'not found: link ' + {id}; }}; DELETE type::record('link', {id});",
                id = v("id")
            )
        }
        OpKind::ViewCreate { view } => {
            bind("id", json!(view.id));
            let mut c = serde_json::to_value(view)?;
            if let Value::Object(o) = &mut c {
                o.remove("id");
            }
            bind("c", c);
            format!("CREATE type::record('view', {}) CONTENT {} RETURN NONE;", v("id"), v("c"))
        }
        OpKind::ViewLayout { id, item, entry } => {
            bind("id", json!(id));
            bind("item", json!(item));
            match entry {
                Some(e) => {
                    bind("patch", json!({ "layout": { item.to_string(): e } }));
                    format!("UPDATE type::record('view', {}) MERGE {} RETURN NONE;", v("id"), v("patch"))
                }
                None => format!(
                    "UPDATE type::record('view', {id}) SET layout = object::from_entries(object::entries(layout).filter(|$e| $e[0] != {item})) RETURN NONE;",
                    id = v("id"),
                    item = v("item")
                ),
            }
        }
    })
}

#[async_trait]
impl<T: HttpTransport + 'static> Store for SurrealHttpStore<T> {
    async fn apply(&self, ops: Vec<Op>) -> Result<Applied> {
        check_ops(&ops)?;
        let batch = Id::new();
        let mut vars = Map::new();
        let mut sql = String::from("BEGIN TRANSACTION;\n");
        for (n, op) in ops.iter().enumerate() {
            sql.push_str(&op_sql(n, op, &mut vars)?);
            sql.push('\n');
            // Ops log row (TODO(M3): capture `before` state for undo).
            vars.insert(format!("p{n}_opid"), json!(Id::new()));
            let after =
                if matches!(op.kind, OpKind::ItemPurge { .. }) { Value::Null } else { serde_json::to_value(&op.kind)? };
            vars.insert(
                format!("p{n}_op"),
                json!({
                    "actor": op.actor.as_log_string(),
                    "kind": op_kind_name(&op.kind),
                    "target": op_target(&op.kind),
                    "batch": batch.to_string(),
                    "after": after,
                }),
            );
            sql.push_str(&format!("CREATE type::record('op', $p{n}_opid) CONTENT $p{n}_op RETURN NONE;\n"));
        }
        sql.push_str("COMMIT TRANSACTION;");

        self.rpc_query(&sql, Value::Object(vars)).await?;
        Ok(Applied { batch, count: ops.len() })
    }

    async fn get_item(&self, id: Id) -> Result<Option<Item>> {
        let v: Value = self
            .query_json(&format!("SELECT {ITEM_PROJECTION} FROM ONLY type::record('item', $id)"), json!({ "id": id }))
            .await?;
        if v.is_null() {
            return Ok(None);
        }
        Ok(Some(serde_json::from_value(v)?))
    }

    async fn query_items(&self, q: &Query, now: DateTime<Utc>) -> Result<Vec<Item>> {
        let mut conds = vec!["lifecycle IN $lc".to_string()];
        let mut vars = Map::new();
        vars.insert("lc".into(), serde_json::to_value(q.effective_lifecycle())?);
        if !q.types.is_empty() {
            conds.push("type IN $types".into());
            vars.insert("types".into(), json!(q.types));
        }
        if !q.tags_all.is_empty() {
            conds.push("tags CONTAINSALL $tags_all".into());
            vars.insert("tags_all".into(), json!(q.tags_all));
        }
        if !q.tags_any.is_empty() {
            conds.push("tags CONTAINSANY $tags_any".into());
            vars.insert("tags_any".into(), json!(q.tags_any));
        }
        if !q.tags_none.is_empty() {
            conds.push("tags CONTAINSNONE $tags_none".into());
            vars.insert("tags_none".into(), json!(q.tags_none));
        }
        if let Some(s) = &q.updated_since {
            let t = reltime::resolve(s, now).map_err(|e| StoreError::InvalidQuery(e.to_string()))?;
            conds.push("updated_at >= <datetime>$since".into());
            vars.insert("since".into(), json!(t.to_rfc3339()));
        }
        if let Some(text) = &q.text {
            conds.push("title @@ $text".into());
            vars.insert("text".into(), json!(text));
        }
        for (i, w) in q.filters.iter().enumerate() {
            let f = safe_field(&w.field)?;
            let var = format!("w{i}");
            let expr = match w.op {
                Cmp::Eq => format!("{f} = ${var}"),
                Cmp::Ne => format!("{f} != ${var}"),
                Cmp::Lt => format!("{f} < ${var}"),
                Cmp::Lte => format!("{f} <= ${var}"),
                Cmp::Gt => format!("{f} > ${var}"),
                Cmp::Gte => format!("{f} >= ${var}"),
                Cmp::In => format!("{f} IN ${var}"),
                Cmp::Contains => format!("{f} CONTAINS ${var}"),
                Cmp::Exists => format!("{f} != NONE"),
            };
            conds.push(expr);
            vars.insert(var, reltime::resolve_json(&w.value, now));
        }
        if let Some(rel) = &q.related_to {
            // depth 1; TODO: deeper traversal with recursive graph paths.
            let kind = if rel.kind.is_some() { " AND kind = $rel_kind" } else { "" };
            conds.push(format!(
                "(id IN (SELECT VALUE out FROM link WHERE in = type::record('item', $rel){kind}) \
                  OR id IN (SELECT VALUE in FROM link WHERE out = type::record('item', $rel){kind}))"
            ));
            vars.insert("rel".into(), json!(rel.item));
            if let Some(k) = &rel.kind {
                vars.insert("rel_kind".into(), json!(k));
            }
        }
        let mut order: Vec<String> = Vec::new();
        for s in &q.sort {
            let dir = if s.dir == SortDir::Desc { "DESC" } else { "ASC" };
            order.push(format!("{} {dir}", safe_field(&s.field)?));
        }
        if order.is_empty() {
            order.push("updated_at DESC".into());
        }
        let limit = q.limit.unwrap_or(500).min(2000);
        let sql = format!(
            "SELECT {ITEM_PROJECTION} FROM item WHERE {} ORDER BY {} LIMIT {limit}",
            conds.join(" AND "),
            order.join(", ")
        );
        self.query_json(&sql, Value::Object(vars)).await
    }

    async fn edges_of(&self, id: Id) -> Result<Vec<Edge>> {
        self.query_json(
            &format!(
                "SELECT {EDGE_PROJECTION} FROM link WHERE in = type::record('item', $id) OR out = type::record('item', $id)"
            ),
            json!({ "id": id }),
        )
        .await
    }

    async fn get_view(&self, id: Id) -> Result<Option<View>> {
        let v: Value = self
            .query_json("SELECT *, record::id(id) AS id FROM ONLY type::record('view', $id)", json!({ "id": id }))
            .await?;
        if v.is_null() {
            return Ok(None);
        }
        Ok(Some(serde_json::from_value(v)?))
    }

    async fn put_capture(&self, capture: Capture) -> Result<()> {
        // Deliberately minimal: no validation of `body` (invariant 1).
        self.rpc_query(
            "CREATE type::record('capture', $id) CONTENT $c RETURN NONE",
            json!({
                "id": capture.id,
                "c": { "source": capture.source, "content_type": capture.content_type, "body": capture.body }
            }),
        )
        .await?;
        Ok(())
    }

    async fn resolve_capture(&self, id: Id, item: Option<Id>, error: Option<String>) -> Result<()> {
        let sql = if item.is_some() {
            "UPDATE type::record('capture', $id) SET resolved = true, item = type::record('item', $item), error = NONE RETURN NONE"
        } else {
            "UPDATE type::record('capture', $id) SET resolved = false, error = $error RETURN NONE"
        };
        self.rpc_query(sql, json!({ "id": id, "item": item, "error": error })).await?;
        Ok(())
    }

    async fn purge_resolved_captures(&self, before: DateTime<Utc>) -> Result<usize> {
        let deleted: Vec<Value> = self
            .query_json(
                "DELETE capture WHERE resolved = true AND received_at < <datetime>$before RETURN BEFORE",
                json!({ "before": before.to_rfc3339() }),
            )
            .await?;
        Ok(deleted.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cairn_domain::{Actor, CaptureSource, EdgeKind, SourceKind};
    use cairn_http::FakeTransport;

    fn store(responses: Vec<cairn_http::HttpResponse>) -> (SurrealHttpStore<FakeTransport>, Arc<FakeTransport>) {
        let t = Arc::new(FakeTransport::with_responses(responses));
        let cfg = SurrealConfig {
            url: "https://db.example".into(),
            namespace: "cairn".into(),
            database: "main".into(),
            username: "u".into(),
            password: "p".into(),
        };
        (SurrealHttpStore::new(t.clone(), cfg), t)
    }

    fn ok(results: Vec<Value>) -> cairn_http::HttpResponse {
        let r: Vec<Value> =
            results.into_iter().map(|v| json!({ "status": "OK", "time": "1ms", "result": v })).collect();
        FakeTransport::json(200, json!({ "id": 1, "result": r }))
    }

    #[tokio::test]
    async fn apply_is_one_transaction_with_bound_vars_and_op_log() {
        let (s, t) = store(vec![ok(vec![])]);
        let a = Item::new("todo", 1, CaptureSource::new(SourceKind::Rest));
        let b = Item::new("bucket", 1, CaptureSource::new(SourceKind::Ui));
        s.apply(vec![
            Op::new(Actor::User, OpKind::ItemCreate { item: a.clone() }),
            Op::new(Actor::User, OpKind::ItemCreate { item: b.clone() }),
            Op::new(Actor::User, OpKind::EdgeCreate { edge: Edge::new(b.id, a.id, EdgeKind::Contains) }),
        ])
        .await
        .unwrap();
        let sent = t.sent();
        assert_eq!(sent.len(), 1, "one HTTP request per batch");
        let req = &sent[0];
        assert!(req.url.ends_with("/rpc"));
        assert!(req.headers.iter().any(|(k, v)| k == "surreal-ns" && v == "cairn"));
        let body = req.body_json().unwrap();
        let sql = body["params"][0].as_str().unwrap();
        assert!(sql.starts_with("BEGIN TRANSACTION;") && sql.trim_end().ends_with("COMMIT TRANSACTION;"));
        assert_eq!(sql.matches("CREATE type::record('op'").count(), 3, "every op is logged");
        assert_eq!(body["params"][1]["p0_id"], json!(a.id.to_string()));
        assert!(!sql.contains(&a.id.to_string()), "ids are bound, never interpolated");
    }

    #[tokio::test]
    async fn real_error_beats_generic_transaction_error() {
        let resp = FakeTransport::json(
            200,
            json!({ "id": 1, "result": [
                { "status": "ERR", "result": "The query was not executed due to a failed transaction" },
                { "status": "ERR", "result": "An error occurred: not found: item 01J..." }
            ]}),
        );
        let (s, _) = store(vec![resp]);
        let err = s.apply(vec![Op::new(Actor::User, OpKind::EdgeDelete { id: Id::new() })]).await.unwrap_err();
        assert!(matches!(err, StoreError::NotFound(_)), "{err:?}");
    }

    #[tokio::test]
    async fn rejects_unsafe_field_names() {
        let (s, t) = store(vec![]);
        let q = Query {
            filters: vec![cairn_domain::Where { field: "x; DELETE item".into(), op: Cmp::Eq, value: json!(1) }],
            ..Default::default()
        };
        assert!(matches!(s.query_items(&q, Utc::now()).await, Err(StoreError::InvalidQuery(_))));
        assert!(t.sent().is_empty(), "nothing sent for invalid queries");
    }

    #[tokio::test]
    async fn get_item_parses_projection() {
        let a = {
            let mut i = Item::new("todo", 1, CaptureSource::new(SourceKind::Rest));
            i.title = "Renew".into();
            i
        };
        let (s, _) = store(vec![ok(vec![serde_json::to_value(&a).unwrap()])]);
        assert_eq!(s.get_item(a.id).await.unwrap().unwrap().title, "Renew");
        let (s, _) = store(vec![ok(vec![Value::Null])]);
        assert!(s.get_item(Id::new()).await.unwrap().is_none());
    }
}
