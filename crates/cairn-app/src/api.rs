//! REST API. Keep handlers thin: auth → parse → service call → map errors.
//! Mounted by `cairn` (hosts/server), which also serves the web app and plugin views.js.

use crate::auth::Scope;
use crate::error::ApiError;
use crate::ingest;
use crate::items::{notify_change, patch_payload};
use crate::state::AppState;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use cairn_domain::{Actor, Edge, EdgeKind, Id, Item, Lifecycle, Op, OpKind, Query};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Map, Value, json};

/// Routes that machines call (phones, scripts, webhooks, Discord) and that therefore bypass
/// the GitHub sign-in in front of Cairn (ADR 0015, `deploy/sign-in.cfg`). Each one checks a
/// credential of its own. Everything else needs the sign-in as well as a token.
/// `hosts/server/tests/sign_in_routes.rs` fails if `sign-in.cfg` and this list disagree.
pub const MACHINE_ROUTES: &[(&str, &str)] = &[
    ("POST", "/api/v1/capture"),
    ("POST", "/ingest/webhook/{source}"),
    ("POST", "/integrations/discord/interactions"),
    ("GET", "/healthz"),
];

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/api/v1/capture", post(ingest::capture_handler))
        .route("/ingest/webhook/{source}", post(ingest::webhook_handler))
        .route("/api/v1/items/{id}", get(get_item).patch(patch_item))
        .route("/api/v1/items/{id}/lifecycle", post(set_lifecycle))
        .route("/api/v1/items/{id}/edges", get(item_edges))
        .route("/api/v1/query", post(query))
        .route("/api/v1/edges", post(create_edge))
        .route("/api/v1/types", get(list_types))
        .route("/api/v1/tasks/{id}/dry-run", post(dry_run))
        .route("/integrations/discord/interactions", post(crate::discord::interactions))
}

fn parse_id(s: &str) -> Result<Id, ApiError> {
    s.parse().map_err(|_| ApiError::bad_request("invalid id"))
}

async fn get_item(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Item>, ApiError> {
    st.tokens.check(&headers, Scope::Read)?;
    let id = parse_id(&id)?;
    st.store.get_item(id).await?.map(Json).ok_or_else(|| ApiError::not_found(id))
}

#[derive(Deserialize)]
struct PatchBody {
    title: Option<String>,
    payload: Option<Map<String, Value>>,
    tags: Option<Vec<String>>,
    pinned: Option<bool>,
}

async fn patch_item(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<PatchBody>,
) -> Result<Json<Item>, ApiError> {
    st.tokens.check(&headers, Scope::Write)?;
    let id = parse_id(&id)?;
    if let Some(p) = body.payload {
        patch_payload(&st, Actor::User, id, p).await?;
    }
    if body.title.is_some() || body.tags.is_some() || body.pinned.is_some() {
        let tags = body.tags.map(|t| t.iter().map(|s| cairn_domain::normalize_tag(s)).collect());
        st.store
            .apply(vec![Op::new(
                Actor::User,
                OpKind::ItemPatch { id, title: body.title, payload: None, tags, pinned: body.pinned, idx: None },
            )])
            .await?;
    }
    let item = st.store.get_item(id).await?.ok_or_else(|| ApiError::not_found(id))?;
    notify_change(&st, "item.patched", id, &item.item_type).await;
    Ok(Json(item))
}

#[derive(Deserialize)]
struct LifecycleBody {
    to: Lifecycle,
}

async fn set_lifecycle(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<LifecycleBody>,
) -> Result<StatusCode, ApiError> {
    st.tokens.check(&headers, Scope::Write)?;
    let id = parse_id(&id)?;
    st.store.apply(vec![Op::new(Actor::User, OpKind::ItemLifecycle { id, to: body.to })]).await?;
    let t = st.store.get_item(id).await?.map(|i| i.item_type).unwrap_or_default();
    notify_change(&st, "item.lifecycle", id, &t).await;
    Ok(StatusCode::NO_CONTENT)
}

async fn item_edges(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Vec<Edge>>, ApiError> {
    st.tokens.check(&headers, Scope::Read)?;
    Ok(Json(st.store.edges_of(parse_id(&id)?).await?))
}

async fn query(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(q): Json<Query>,
) -> Result<Json<Vec<Item>>, ApiError> {
    st.tokens.check(&headers, Scope::Read)?;
    Ok(Json(st.store.query_items(&q, Utc::now()).await?))
}

#[derive(Deserialize)]
struct EdgeBody {
    from: Id,
    to: Id,
    kind: EdgeKind,
    #[serde(default)]
    props: Map<String, Value>,
}

async fn create_edge(
    State(st): State<AppState>,
    headers: HeaderMap,
    Json(b): Json<EdgeBody>,
) -> Result<(StatusCode, Json<Edge>), ApiError> {
    st.tokens.check(&headers, Scope::Write)?;
    let mut edge = Edge::new(b.from, b.to, b.kind);
    edge.props = b.props;
    st.store.apply(vec![Op::new(Actor::User, OpKind::EdgeCreate { edge: edge.clone() })]).await?;
    notify_change(&st, "edge.created", edge.from, "").await;
    Ok((StatusCode::CREATED, Json(edge)))
}

async fn list_types(State(st): State<AppState>, headers: HeaderMap) -> Result<Json<Value>, ApiError> {
    st.tokens.check(&headers, Scope::Read)?;
    let types: Vec<Value> = st
        .plugins
        .type_names()
        .filter_map(|n| st.plugins.get_type(n))
        .map(|t| {
            json!({
                "name": t.manifest.name,
                "version": t.manifest.version,
                "title": t.manifest.title,
                "icon": t.manifest.icon,
                "index": t.manifest.index,
                "actions": t.manifest.actions,
                "views": t.manifest.views.as_ref().map(|v| format!("/plugins/types/{}/{v}", t.manifest.name)),
            })
        })
        .collect();
    Ok(Json(json!({ "types": types })))
}

async fn dry_run(
    State(st): State<AppState>,
    headers: HeaderMap,
    Path(_id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    st.tokens.check(&headers, Scope::Write)?;
    Err(ApiError::not_implemented("task dry-run (M5)"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{call, harness};
    use axum::body::Body;
    use axum::http::Request;

    #[tokio::test]
    async fn capture_plans_item_spools_then_consumer_persists() {
        let h = harness();
        let app = router().with_state(h.state.clone());

        let req = Request::post("/api/v1/capture")
            .header("authorization", "Bearer cap")
            .header("content-type", "text/plain")
            .body(Body::from("todo: renew registration"))
            .unwrap();
        let (status, body) = call(app.clone(), req).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(body["type"], "todo");
        let id: Id = body["item_id"].as_str().unwrap().parse().unwrap();
        assert!(h.state.store.get_item(id).await.unwrap().is_none(), "not written until the consumer runs");

        for msg in h.spool.drain() {
            crate::ingest::process_capture(&h.state, &msg).await.unwrap();
            // redelivery is harmless
            crate::ingest::process_capture(&h.state, &msg).await.unwrap();
        }
        let item = h.state.store.get_item(id).await.unwrap().unwrap();
        assert_eq!(item.title, "renew registration");
        assert_eq!(h.hub.0.lock().unwrap().len(), 2, "one change event per delivery");
    }

    #[tokio::test]
    async fn json_capture_falls_back_to_inbox_with_normalized_tags() {
        let h = harness();
        let app = router().with_state(h.state.clone());
        let req = Request::post("/api/v1/capture")
            .header("authorization", "Bearer cap")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"text":"what if buckets could nest","tags":["Ideas"]}"#))
            .unwrap();
        let (_, body) = call(app, req).await;
        assert_eq!(body["type"], "hold-that-thought");
        let msg = h.spool.drain().pop().unwrap();
        assert_eq!(msg.item.unwrap().tags, vec!["ideas", "unsorted"]);
    }

    #[tokio::test]
    async fn capture_token_cannot_read_items() {
        let h = harness();
        let app = router().with_state(h.state);
        let req = Request::post("/api/v1/query")
            .header("authorization", "Bearer cap")
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();
        let (status, body) = call(app, req).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["error"]["code"], "forbidden");
    }

    #[tokio::test]
    async fn empty_capture_is_spooled_not_rejected() {
        let h = harness();
        let app = router().with_state(h.state.clone());
        let req = Request::post("/api/v1/capture").header("authorization", "Bearer cap").body(Body::from("")).unwrap();
        let (status, body) = call(app, req).await;
        assert_eq!(status, StatusCode::ACCEPTED, "invariant 1: never reject after auth");
        assert!(body["item_id"].is_null());
        let msg = h.spool.drain().pop().unwrap();
        crate::ingest::process_capture(&h.state, &msg).await.unwrap();
        assert!(h.store.capture(msg.capture.id).is_some(), "raw capture persisted even with no item");
    }

    #[tokio::test]
    async fn patching_a_reminder_rearms_the_scheduler_and_updates_idx() {
        let h = harness();
        let app = router().with_state(h.state.clone());
        let req = Request::post("/api/v1/capture")
            .header("authorization", "Bearer cap")
            .header("content-type", "application/json")
            .body(Body::from(
                r#"{"type":"reminder","payload":{"message":"dmv","fire_at":"2030-01-01T09:00:00Z","state":"pending"}}"#,
            ))
            .unwrap();
        let (_, body) = call(app.clone(), req).await;
        let id = body["item_id"].as_str().unwrap().to_string();
        for m in h.spool.drain() {
            crate::ingest::process_capture(&h.state, &m).await.unwrap();
        }
        let before = h.scheduler.0.load(std::sync::atomic::Ordering::SeqCst);
        let req = Request::patch(format!("/api/v1/items/{id}"))
            .header("authorization", "Bearer ui")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"payload":{"state":"cancelled"}}"#))
            .unwrap();
        let (status, body) = call(app, req).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["idx"]["state"], "cancelled", "idx follows payload");
        assert!(h.scheduler.0.load(std::sync::atomic::Ordering::SeqCst) > before);
    }
}
