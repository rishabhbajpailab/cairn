//! `SurrealHttpStore` against a real SurrealDB. Skipped unless `CAIRN_SURREAL_URL` is set:
//!
//!   make db                                   # or any SurrealDB 3.x/4.x
//!   CAIRN_SURREAL_URL=http://127.0.0.1:8000 cargo test -p cairn-store --test live_surreal -- --nocapture
//!
//! Optional: `CAIRN_SURREAL_USER` / `CAIRN_SURREAL_PASS` (default root/root),
//! `CAIRN_SURREAL_AUTH` (`root` | `namespace`, default root; the test creates a database),
//! `CAIRN_SURREAL_NS` (default `cairn_test`). Each run uses a fresh database, `t<ulid>`, and
//! removes it afterwards unless `CAIRN_SURREAL_KEEP=1`.
//!
//! The transport is a blocking `ureq` client. That's fine in a test, and keeps tokio a
//! dev-dependency only.

use async_trait::async_trait;
use cairn_domain::query::RelatedTo;
use cairn_domain::reltime;
use cairn_domain::{
    Actor, Capture, CaptureSource, Cmp, Edge, EdgeKind, Id, Item, LayoutEntry, Lifecycle, Op, OpKind, Query, SortDir,
    SortKey, SourceKind, View, ViewKind, Where,
};
use cairn_http::{HttpError, HttpRequest, HttpResponse, HttpTransport};
use cairn_store::{AuthLevel, SchemaFile, Store, StoreError, SurrealConfig, SurrealHttpStore};
use chrono::Utc;
use serde_json::{Map, Value, json};
use std::sync::Arc;

struct Ureq(ureq::Agent);

#[async_trait]
impl HttpTransport for Ureq {
    async fn send(&self, req: HttpRequest) -> Result<HttpResponse, HttpError> {
        let mut b = ureq::http::Request::builder().method(req.method).uri(&req.url);
        for (k, v) in &req.headers {
            b = b.header(k, v);
        }
        let r = b.body(req.body.unwrap_or_default()).map_err(|e| HttpError::Encode(e.to_string()))?;
        let mut resp = self.0.run(r).map_err(|e| HttpError::Network(e.to_string()))?;
        let status = resp.status().as_u16();
        let body = resp.body_mut().read_to_vec().map_err(|e| HttpError::Network(e.to_string()))?;
        Ok(HttpResponse { status, body })
    }
}

fn env(k: &str, default: &str) -> String {
    std::env::var(k).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| default.into())
}

fn connect(url: &str, database: &str) -> SurrealHttpStore<Ureq> {
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
    SurrealHttpStore::new(
        Arc::new(Ureq(agent)),
        SurrealConfig {
            url: url.into(),
            namespace: env("CAIRN_SURREAL_NS", "cairn_test"),
            database: database.into(),
            username: env("CAIRN_SURREAL_USER", "root"),
            password: env("CAIRN_SURREAL_PASS", "root"),
            auth: AuthLevel::parse(&env("CAIRN_SURREAL_AUTH", "root"))
                .expect("CAIRN_SURREAL_AUTH: root|namespace|database"),
        },
    )
}

fn schema_files() -> Vec<(String, String)> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../schema");
    let mut files: Vec<(String, String)> = std::fs::read_dir(dir)
        .expect("schema dir")
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "surql"))
        .map(|e| {
            (e.file_name().to_string_lossy().into_owned(), std::fs::read_to_string(e.path()).expect("read schema"))
        })
        .collect();
    files.sort();
    files
}

fn obj(v: Value) -> Map<String, Value> {
    v.as_object().cloned().expect("object")
}

fn todo(title: &str, tags: &[&str], idx: Value) -> Item {
    let mut i = Item::new("todo", 1, CaptureSource::new(SourceKind::Rest));
    i.title = title.into();
    i.payload = obj(json!({ "text": title }));
    i.idx = obj(idx);
    i.tags = tags.iter().map(|t| t.to_string()).collect();
    i
}

fn user(kind: OpKind) -> Op {
    Op::new(Actor::User, kind)
}

async fn count(s: &SurrealHttpStore<Ureq>, sql: &str, vars: Value) -> i64 {
    let r = s.rpc_query(sql, vars).await.expect(sql);
    r[0].as_i64().unwrap_or_else(|| panic!("{sql}: expected a count, got {:?}", r[0]))
}

#[tokio::test]
async fn store_contract_against_live_surreal() {
    let Ok(url) = std::env::var("CAIRN_SURREAL_URL") else {
        eprintln!("skipped: set CAIRN_SURREAL_URL to run against a live SurrealDB");
        return;
    };
    let db = format!("t{}", Id::new().to_string().to_lowercase());
    let s = connect(&url, &db);
    // SurrealDB 3+/4 doesn't create a namespace or database implicitly.
    let ns = env("CAIRN_SURREAL_NS", "cairn_test");
    s.rpc_query(
        &format!("DEFINE NAMESPACE IF NOT EXISTS {ns}; USE NS {ns}; DEFINE DATABASE IF NOT EXISTS {db};"),
        json!({}),
    )
    .await
    .expect("create namespace and database (needs a root or namespace user)");
    eprintln!("using {url} ns={} db={db}", env("CAIRN_SURREAL_NS", "cairn_test"));

    let result = run(&s).await;
    if env("CAIRN_SURREAL_KEEP", "") != "1" {
        let _ = s.rpc_query(&format!("REMOVE DATABASE IF EXISTS {db}"), json!({})).await;
    }
    result.expect("live contract");
}

/// Each step panics with context on failure; the Result only carries setup errors.
async fn run(s: &SurrealHttpStore<Ureq>) -> Result<(), StoreError> {
    let now = Utc::now();

    // ── migrations: applied once, recorded in _migration ──
    let files = schema_files();
    let refs: Vec<SchemaFile> = files.iter().map(|(n, q)| SchemaFile { name: n, sql: q }).collect();
    let ran = s.migrate(&refs).await?;
    assert_eq!(ran, files.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>(), "first migrate applies every file");
    assert!(s.migrate(&refs).await?.is_empty(), "second migrate is a no-op");

    // ── create: items, an edge and a view in one transaction ──
    let a = todo("Renew passport registration", &["admin", "urgent"], json!({ "done": false, "priority": 2 }));
    let b = {
        let mut i = Item::new("bucket", 1, CaptureSource::new(SourceKind::Ui));
        i.title = "Paperwork".into();
        i.payload = obj(json!({ "name": "Paperwork" }));
        i
    };
    let c = todo(
        "Buy milk",
        &["errand"],
        json!({ "done": true, "priority": 1, "due": reltime::canonical(now + chrono::Duration::hours(3)) }),
    );
    let edge = Edge::new(b.id, a.id, EdgeKind::Contains);
    let view = View {
        id: Id::new(),
        kind: ViewKind::Canvas,
        name: "Main".into(),
        query: Query::default(),
        layout: [(a.id, LayoutEntry { x: 10.0, y: 20.0, w: None, h: None, collapsed: false })].into_iter().collect(),
        config: Map::new(),
    };
    let applied = s
        .apply(vec![
            user(OpKind::ItemCreate { item: a.clone() }),
            user(OpKind::ItemCreate { item: b.clone() }),
            user(OpKind::ItemCreate { item: c.clone() }),
            user(OpKind::EdgeCreate { edge: edge.clone() }),
            user(OpKind::ViewCreate { view: view.clone() }),
        ])
        .await?;
    assert_eq!(applied.count, 5);
    assert_eq!(
        count(s, "RETURN count(SELECT id FROM op WHERE batch = $b)", json!({ "b": applied.batch.to_string() })).await,
        5,
        "every op is logged under the batch id"
    );

    // ── read back ──
    let got = s.get_item(a.id).await?.expect("item a exists");
    assert_eq!(got.id, a.id);
    assert_eq!(got.title, a.title);
    assert_eq!(got.payload, a.payload);
    assert_eq!(got.idx, a.idx);
    assert_eq!(got.tags, a.tags);
    assert_eq!(got.lifecycle, Lifecycle::Active);
    assert!(got.created_at <= Utc::now() && got.created_at > now - chrono::Duration::minutes(5));
    assert!(s.get_item(Id::new()).await?.is_none(), "missing item is None");

    let v = s.get_view(view.id).await?.expect("view exists");
    assert_eq!(v.layout.get(&a.id).map(|e| (e.x, e.y)), Some((10.0, 20.0)));

    let edges = s.edges_of(a.id).await?;
    assert_eq!(edges.len(), 1, "{edges:?}");
    assert_eq!((edges[0].id, edges[0].from, edges[0].to), (edge.id, b.id, a.id));
    assert_eq!(edges[0].kind, EdgeKind::Contains);

    // ── queries ──
    let ids = |items: Vec<Item>| items.into_iter().map(|i| i.id).collect::<Vec<_>>();
    let q = |f: fn(&mut Query)| {
        let mut q = Query::default();
        f(&mut q);
        q
    };
    assert_eq!(ids(s.query_items(&q(|q| q.types = vec!["bucket".into()]), now).await?), vec![b.id]);
    assert_eq!(ids(s.query_items(&q(|q| q.tags_all = vec!["admin".into(), "urgent".into()]), now).await?), vec![a.id]);
    assert_eq!(ids(s.query_items(&q(|q| q.tags_any = vec!["errand".into()]), now).await?), vec![c.id]);
    let none = s
        .query_items(
            &q(|q| {
                q.types = vec!["todo".into()];
                q.tags_none = vec!["admin".into()]
            }),
            now,
        )
        .await?;
    assert_eq!(ids(none), vec![c.id]);
    let filtered = q(|q| {
        q.filters = vec![Where { field: "priority".into(), op: Cmp::Gte, value: json!(2) }];
    });
    assert_eq!(ids(s.query_items(&filtered, now).await?), vec![a.id], "idx filter");
    let sorted = q(|q| {
        q.types = vec!["todo".into()];
        q.sort = vec![SortKey { field: "idx.priority".into(), dir: SortDir::Asc }];
    });
    assert_eq!(ids(s.query_items(&sorted, now).await?), vec![c.id, a.id], "idx sort");
    assert_eq!(ids(s.query_items(&q(|q| q.text = Some("passport".into())), now).await?), vec![a.id], "FULLTEXT");
    let due = |v: &'static str, op: Cmp| Query {
        filters: vec![Where { field: "due".into(), op, value: json!(v) }],
        ..Default::default()
    };
    assert_eq!(ids(s.query_items(&due("+1d", Cmp::Lt), now).await?), vec![c.id], "datetime idx vs relative time");
    assert!(s.query_items(&due("+1h", Cmp::Lt), now).await?.is_empty(), "datetime idx vs relative time");
    assert_eq!(ids(s.query_items(&q(|q| q.updated_since = Some("-1h".into())), now).await?).len(), 3, "updated_since");
    let rel = Query {
        related_to: Some(RelatedTo { item: b.id, kind: Some("contains".into()), depth: 1 }),
        ..Default::default()
    };
    assert_eq!(ids(s.query_items(&rel, now).await?), vec![a.id], "related_to");

    // ── constraints ──
    let dup = s.apply(vec![user(OpKind::EdgeCreate { edge: Edge::new(b.id, a.id, EdgeKind::Contains) })]).await;
    assert!(matches!(dup, Err(StoreError::Conflict(_))), "duplicate edge: {dup:?}");
    let missing = s.apply(vec![user(OpKind::EdgeDelete { id: Id::new() })]).await;
    assert!(matches!(missing, Err(StoreError::NotFound(_))), "missing edge: {missing:?}");

    // A failing op rolls back the whole batch, op-log rows included.
    let d = todo("Should not exist", &[], json!({}));
    let bad = s
        .apply(vec![
            user(OpKind::ItemCreate { item: d.clone() }),
            user(OpKind::ItemPatch {
                id: Id::new(),
                title: Some("x".into()),
                payload: None,
                tags: None,
                pinned: None,
                idx: None,
            }),
        ])
        .await;
    assert!(matches!(bad, Err(StoreError::NotFound(_))), "patch of missing item: {bad:?}");
    assert!(s.get_item(d.id).await?.is_none(), "batch is atomic");
    assert_eq!(
        count(s, "RETURN count(SELECT id FROM op WHERE target = $t)", json!({ "t": format!("item:{}", d.id) })).await,
        0,
        "no op-log rows from a failed batch"
    );

    // ── patch, layout, lifecycle ──
    s.apply(vec![user(OpKind::ItemPatch {
        id: a.id,
        title: Some("Renew passport".into()),
        payload: Some(obj(json!({ "text": "Renew passport", "done": true }))),
        tags: None,
        pinned: Some(true),
        idx: Some(obj(json!({ "done": true, "priority": 2 }))),
    })])
    .await?;
    let got = s.get_item(a.id).await?.expect("a");
    assert_eq!(got.title, "Renew passport");
    assert_eq!(got.payload["done"], json!(true));
    assert_eq!(got.idx["done"], json!(true));
    assert!(got.pinned);
    assert_eq!(got.tags, a.tags, "untouched fields stay");
    assert!(got.updated_at >= got.created_at);

    let entry = LayoutEntry { x: 1.0, y: 2.0, w: Some(3.0), h: None, collapsed: false };
    s.apply(vec![user(OpKind::ViewLayout { id: view.id, item: b.id, entry: Some(entry.clone()) })]).await?;
    s.apply(vec![user(OpKind::ViewLayout { id: view.id, item: a.id, entry: None })]).await?;
    let v = s.get_view(view.id).await?.expect("view");
    assert_eq!(v.layout.keys().copied().collect::<Vec<_>>(), vec![b.id], "layout set + remove");

    s.apply(vec![user(OpKind::ItemLifecycle { id: c.id, to: Lifecycle::Trashed })]).await?;
    let got = s.get_item(c.id).await?.expect("c");
    assert_eq!(got.lifecycle, Lifecycle::Trashed);
    assert!(got.trashed_at.is_some());
    assert!(!ids(s.query_items(&Query::default(), now).await?).contains(&c.id), "active excludes trashed");
    let trashed = Query { lifecycle: vec![Lifecycle::Trashed], ..Default::default() };
    assert_eq!(ids(s.query_items(&trashed, now).await?), vec![c.id]);
    s.apply(vec![user(OpKind::ItemLifecycle { id: c.id, to: Lifecycle::Active })]).await?;
    let got = s.get_item(c.id).await?.expect("c");
    assert!(got.trashed_at.is_none() && got.lifecycle == Lifecycle::Active, "restore clears trashed_at");

    // ── captures: raw, idempotent by id ──
    let cap = Capture::new(CaptureSource::new(SourceKind::Webhook), "application/json", json!({ "any": ["shape", 1] }));
    s.put_capture(cap.clone()).await?;
    let again = s.put_capture(cap.clone()).await;
    assert!(matches!(again, Err(StoreError::Conflict(_))), "re-delivered capture is a Conflict: {again:?}");
    let odd = Capture::new(CaptureSource::new(SourceKind::Rest), "text/plain", json!("just text"));
    s.put_capture(odd.clone()).await?;
    s.resolve_capture(cap.id, Some(a.id), None).await?;
    s.resolve_capture(odd.id, None, Some("no matcher".into())).await?;
    let purged = s.purge_resolved_captures(Utc::now() + chrono::Duration::seconds(5)).await?;
    assert_eq!(purged, 1, "only resolved captures are purged");

    // ── strings stay strings (JSON-RPC would turn these into a record id and a datetime) ──
    let tricky = {
        let mut i = todo("note:abc", &[], json!({}));
        i.payload =
            obj(json!({ "text": "note:abc", "nested": { "when": "2026-01-01T00:00:00Z", "refs": ["item:01J"] } }));
        i
    };
    s.apply(vec![user(OpKind::ItemCreate { item: tricky.clone() })]).await?;
    let types = s
        .rpc_query(
            "LET $i = type::record('item', $id); RETURN [type::of($i.title), type::of($i.payload.text), \
             type::of($i.payload.nested.when), type::of($i.payload.nested.refs[0])];",
            json!({ "id": tricky.id }),
        )
        .await?;
    assert_eq!(types.last(), Some(&json!(["string", "string", "string", "string"])), "payload strings retyped");
    assert_eq!(s.get_item(tricky.id).await?.expect("tricky").payload, tricky.payload);
    let raw = Capture::new(CaptureSource::new(SourceKind::Rest), "application/json", json!({ "x": "user:1" }));
    s.put_capture(raw.clone()).await?;
    let t = s.rpc_query("RETURN type::of(type::record('capture', $id).body.x)", json!({ "id": raw.id })).await?;
    assert_eq!(t[0], json!("string"), "raw capture bodies are stored unchanged (invariant 1)");

    // ── ops log is append-only (op_append_only event) ──
    let upd = s
        .rpc_query("UPDATE op SET kind = 'tampered' WHERE batch = $b", json!({ "b": applied.batch.to_string() }))
        .await;
    assert!(upd.is_err(), "ops log UPDATE must be rejected, got {upd:?}");
    let del = s.rpc_query("DELETE op WHERE batch = $b", json!({ "b": applied.batch.to_string() })).await;
    assert!(del.is_err(), "ops log DELETE must be rejected, got {del:?}");

    // ── purge: GC only, cascades to edges, layouts and op-log payloads ──
    let not_gc = s.apply(vec![user(OpKind::ItemPurge { id: a.id })]).await;
    assert!(matches!(not_gc, Err(StoreError::Forbidden(_))));
    s.apply(vec![user(OpKind::ViewLayout { id: view.id, item: a.id, entry: Some(entry) })]).await?;
    s.apply(vec![Op::new(Actor::Gc, OpKind::ItemPurge { id: a.id })]).await?;
    assert!(s.get_item(a.id).await?.is_none(), "purged");
    assert!(s.edges_of(b.id).await?.is_empty(), "edges cascade");
    assert!(!s.get_view(view.id).await?.expect("view").layout.contains_key(&a.id), "layout cascade");
    let target = json!({ "t": format!("item:{}", a.id) });
    assert!(count(s, "RETURN count(SELECT id FROM op WHERE target = $t)", target.clone()).await >= 3, "op rows kept");
    assert_eq!(
        count(s, "RETURN count(SELECT id FROM op WHERE target = $t AND (after != NONE OR before != NONE))", target)
            .await,
        0,
        "op-log payloads redacted"
    );

    eprintln!("live SurrealDB contract: all checks passed");
    Ok(())
}
