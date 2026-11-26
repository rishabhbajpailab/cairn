//! capture → spool (Queue) → consumer → store (docs/ARCHITECTURE.md § Data flow).
//!
//! Invariant 1: once auth has passed and the Queue has accepted the message, the capture is
//! durable. Resolution is pure and runs *before* enqueue so the reply can say what the
//! capture will become; the consumer applies it idempotently.

use crate::auth::Scope;
use crate::error::ApiError;
use crate::items::{notify_change, recompute};
use crate::ports::CaptureMsg;
use crate::state::AppState;
use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use cairn_domain::{Actor, Capture, CaptureSource, Id, Item, Op, OpKind, SourceKind, normalize_tag};
use cairn_store::StoreError;
use serde::Serialize;
use serde_json::{Map, Value, json};

/// Fallback type for anything nothing else claims.
pub const INBOX_TYPE: &str = "hold-that-thought";

#[derive(Debug, Clone, Serialize)]
pub struct Planned {
    pub id: Id,
    pub item_type: String,
    pub title: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Ingested {
    pub capture_id: Id,
    pub planned: Option<Planned>,
}

pub async fn capture_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let token = state.tokens.check(&headers, Scope::Capture)?;
    let content_type = header(&headers, "content-type").unwrap_or("text/plain").to_string();
    let body = parse_body(&content_type, &body);
    let mut source = CaptureSource::new(SourceKind::Rest);
    source.label = Some(token);
    let r = ingest(&state, source, content_type, body).await?;
    Ok((StatusCode::ACCEPTED, Json(response(&r))))
}

pub async fn webhook_handler(
    State(state): State<AppState>,
    Path(source_name): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let token = header(&headers, "x-cairn-token").ok_or_else(ApiError::unauthorized)?;
    let digest = crate::auth::sha256_hex(token);
    let src = state
        .cfg
        .webhooks
        .iter()
        .find(|w| w.source == source_name && w.token_sha256.eq_ignore_ascii_case(&digest))
        .ok_or_else(ApiError::unauthorized)?;
    let content_type = header(&headers, "content-type").unwrap_or("application/json").to_string();
    let raw = parse_body(&content_type, &body);
    // TODO(M1): apply `src.map` (JSONPath) → {title, text, tags, type}. Unmapped → tag `unmapped`.
    let mapped = json!({ "text": raw.to_string(), "tags": ["unmapped", src.source] });
    let mut source = CaptureSource::new(SourceKind::Webhook);
    source.reference = Some(src.source.clone());
    let r = ingest(&state, source, content_type, mapped).await?;
    Ok((StatusCode::ACCEPTED, Json(response(&r))))
}

fn response(r: &Ingested) -> Value {
    json!({
        "capture_id": r.capture_id,
        "item_id": r.planned.as_ref().map(|p| p.id),
        "type": r.planned.as_ref().map(|p| p.item_type.clone()),
    })
}

fn header<'a>(h: &'a HeaderMap, name: &str) -> Option<&'a str> {
    h.get(name).and_then(|v| v.to_str().ok())
}

fn parse_body(content_type: &str, body: &[u8]) -> Value {
    if content_type.starts_with("application/json")
        && let Ok(v) = serde_json::from_slice::<Value>(body)
    {
        return v;
    }
    Value::String(String::from_utf8_lossy(body).into_owned())
}

/// Plan the item (pure) and hand the capture to the durable spool.
/// The ONLY failure mode is the spool itself being unavailable (503).
pub async fn ingest(
    state: &AppState,
    source: CaptureSource,
    content_type: String,
    body: Value,
) -> Result<Ingested, ApiError> {
    let capture = Capture::new(source, content_type, body);
    let (item, error) = match resolve(state, &capture) {
        Ok(item) => (Some(item), None),
        Err(e) => (None, Some(e)),
    };
    let planned = item.as_ref().map(|i| Planned { id: i.id, item_type: i.item_type.clone(), title: i.title.clone() });
    let msg = CaptureMsg { capture, item, error };
    state.spool.enqueue(&msg).await.map_err(|e| {
        tracing::error!(error = %e, "spool unavailable");
        ApiError::new(StatusCode::SERVICE_UNAVAILABLE, "spool_unavailable", "capture queue unavailable; retry")
    })?;
    Ok(Ingested { capture_id: msg.capture.id, planned })
}

/// Queue consumer body: persist the raw capture and the planned item. Idempotent: a
/// redelivered message hits Conflict on both writes, which counts as already done.
pub async fn process_capture(state: &AppState, msg: &CaptureMsg) -> Result<(), StoreError> {
    let ok_if_conflict = |r: Result<(), StoreError>| match r {
        Err(StoreError::Conflict(_)) => Ok(()),
        other => other,
    };
    ok_if_conflict(state.store.put_capture(msg.capture.clone()).await)?;
    match &msg.item {
        Some(item) => {
            let actor = Actor::Ingest(format!("{:?}", msg.capture.source.kind).to_lowercase());
            let r = state.store.apply(vec![Op::new(actor, OpKind::ItemCreate { item: item.clone() })]).await;
            ok_if_conflict(r.map(|_| ()))?;
            state.store.resolve_capture(msg.capture.id, Some(item.id), None).await?;
            notify_change(state, "item.created", item.id, &item.item_type).await;
        }
        None => {
            state.store.resolve_capture(msg.capture.id, None, msg.error.clone()).await?;
        }
    }
    Ok(())
}

/// Deterministic resolution: explicit type → plugin matchers → inbox.
/// (The async `on-ingest` triage task runs later and proposes changes; it never blocks this.)
pub fn resolve(state: &AppState, capture: &Capture) -> Result<Item, String> {
    let obj = capture.body.as_object();
    let text = capture.text().unwrap_or_default().to_string();
    let mut tags: Vec<String> = obj
        .and_then(|o| o.get("tags"))
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(normalize_tag).collect())
        .unwrap_or_default();

    // 1. explicit type
    if let Some(t) = obj.and_then(|o| o.get("type")).and_then(Value::as_str) {
        let payload = obj
            .and_then(|o| o.get("payload"))
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_else(|| text_payload(&text));
        if state.plugins.validate(t, &payload).is_ok() {
            return Ok(build(state, capture, t, payload, tags));
        }
        tags.push("invalid-payload".into());
    }

    // 2. declarative plugin matchers
    if let Some((t, payload)) = state.plugins.match_capture(&text)
        && state.plugins.validate(&t, &payload).is_ok()
    {
        return Ok(build(state, capture, &t, payload, tags));
    }

    // 3. inbox fallback
    if text.trim().is_empty() {
        return Err("capture has no text and no valid typed payload".into());
    }
    tags.push("unsorted".into());
    Ok(build(state, capture, INBOX_TYPE, text_payload(&text), tags))
}

fn text_payload(text: &str) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert("text".into(), Value::String(text.to_string()));
    m
}

fn build(
    state: &AppState,
    capture: &Capture,
    type_name: &str,
    payload: Map<String, Value>,
    mut tags: Vec<String>,
) -> Item {
    let version = state.plugins.get_type(type_name).map(|t| t.manifest.version).unwrap_or(1);
    let mut source = capture.source.clone();
    source.reference = Some(format!("capture:{}", capture.id));
    let mut item = Item::new(type_name, version, source);
    item.payload = payload;
    recompute(state, &mut item);
    tags.sort();
    tags.dedup();
    item.tags = tags;
    item
}
