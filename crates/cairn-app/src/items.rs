//! Item mutations that need plugin knowledge (validation, idx/title recompute) plus
//! change notification. Handlers and background jobs go through these, never raw ops.

use crate::error::ApiError;
use crate::ports::ChangeEvent;
use crate::state::AppState;
use axum::http::StatusCode;
use cairn_domain::{Actor, Id, Item, Op, OpKind};
use serde_json::{Map, Value};

/// Types whose changes can move the Scheduler's next alarm.
const SCHEDULED_TYPES: &[&str] = &["reminder", "task"];

/// Recompute derived fields (title, idx) from the payload.
pub fn recompute(state: &AppState, item: &mut Item) {
    if let Some(t) = state.plugins.derive_title(&item.item_type, &item.payload) {
        item.title = t;
    }
    item.idx = state.plugins.extract_idx(&item.item_type, &item.payload);
}

pub async fn notify_change(state: &AppState, kind: &str, id: Id, item_type: &str) {
    state.hub.publish(&ChangeEvent { kind: kind.into(), id }).await;
    if SCHEDULED_TYPES.contains(&item_type) {
        state.scheduler.rearm().await;
    }
}

/// Shallow-merge `partial` into the payload, validate against the plugin schema, recompute
/// idx/title, and apply as one op.
pub async fn patch_payload(
    state: &AppState,
    actor: Actor,
    id: Id,
    partial: Map<String, Value>,
) -> Result<Item, ApiError> {
    let current = state.store.get_item(id).await?.ok_or_else(|| ApiError::not_found(id))?;
    let mut next = current.clone();
    next.payload.extend(partial.clone());
    state
        .plugins
        .validate(&next.item_type, &next.payload)
        .map_err(|errs| ApiError::new(StatusCode::UNPROCESSABLE_ENTITY, "invalid_payload", errs.join("; ")))?;
    recompute(state, &mut next);
    let title = (next.title != current.title).then(|| next.title.clone());
    state
        .store
        .apply(vec![Op::new(
            actor,
            OpKind::ItemPatch {
                id,
                title,
                payload: Some(partial),
                tags: None,
                pinned: None,
                idx: Some(next.idx.clone()),
            },
        )])
        .await?;
    notify_change(state, "item.patched", id, &next.item_type).await;
    Ok(next)
}
