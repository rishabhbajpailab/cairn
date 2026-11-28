//! The built-in "Getting started" checklist: a bucket plus setup to-dos, linked by
//! `contains` edges and tagged [`TAG`]. The web UI offers it on an empty Cairn
//! (`POST /api/v1/onboarding`). Seeding is idempotent: if any item carries the tag, in
//! any lifecycle, nothing is written, so archiving or trashing the checklist keeps it gone.
//!
//! The steps are ordinary `todo` items. They go through plugin validation and
//! `Store::apply` like everything else (invariant 2), and GC archives them once done.

use crate::error::ApiError;
use crate::items::{notify_change, recompute};
use crate::state::AppState;
use axum::http::StatusCode;
use cairn_domain::{Actor, CaptureSource, Edge, EdgeKind, Id, Item, Lifecycle, Op, OpKind, Query, SourceKind};
use chrono::{Duration, Utc};
use serde::Serialize;
use serde_json::{Map, Value, json};

/// Tag on the checklist bucket and every step.
pub const TAG: &str = "onboarding";

pub const BUCKET_NAME: &str = "Getting started";

/// (text, notes, priority). Order matters: the UI shows steps oldest first.
pub const STEPS: &[(&str, &str, i64)] = &[
    (
        "Capture your first thought",
        "Type into the capture box at the top of the inbox. Start with \"todo:\" for a to-do or \"htt:\" to park a thought for later. Anything else lands in the inbox tagged #unsorted.",
        2,
    ),
    (
        "Give your phone a capture-only token",
        "Pick a long random token, run make hash to get its sha256, and add phone:capture:<sha256> to CAIRN_TOKENS in deploy/cairn.env, then recreate the cairn container. A capture token can only POST /api/v1/capture, so it is safe on a phone. See docs/ingest.md.",
        2,
    ),
    (
        "Set up the iOS Shortcut",
        "Ask for Input, then Get Contents of URL: POST https://<your host>/api/v1/capture with the header \"Authorization: Bearer <capture token>\". Put it on the Action Button or the lock screen. See docs/ingest.md.",
        1,
    ),
    (
        "Check the timezone and quiet hours",
        "config/cairn.toml sets timezone (used by reminders, cron tasks and digests) and [notify] quiet_hours. On the VPS, copy it, point CAIRN_CONFIG at your copy and recreate the container.",
        1,
    ),
    (
        "Connect Discord for reminders",
        "Create a Discord application, put DISCORD_PUBLIC_KEY and DISCORD_BOT_TOKEN in deploy/cairn.env, set [discord] enabled = true and your dm_user_id, and set the interactions endpoint to https://<your host>/integrations/discord/interactions. See docs/deploy.md step 9.",
        1,
    ),
    (
        "Set spend limits before turning on AI",
        "Give Cairn its own OpenRouter key with a credit limit, store it in Cloudflare AI Gateway (BYOK, alias cairn), add a spend-limit rule, and put only CF_AIG_TOKEN in deploy/gateway.env. Caps live in config/gateway.toml. See docs/spend-control.md.",
        1,
    ),
    (
        "Run and check a backup",
        "Run make vps-backup-upload once, check the object arrived in the bucket, then add the weekly cron line from docs/deploy.md. Cairn's namespace can't be rebuilt, so this one matters.",
        2,
    ),
    (
        "Link two items on the canvas",
        "Open Canvas, choose a link kind in the toolbar, then drag from the dot on a card's right edge to another card.",
        0,
    ),
];

#[derive(Debug, Clone, Serialize)]
pub struct Seeded {
    /// false when a checklist already existed and nothing was written
    pub created: bool,
    pub bucket: Option<Id>,
    pub steps: usize,
}

/// Has a checklist ever been seeded (and not yet purged)?
pub async fn existing(state: &AppState) -> Result<Option<Item>, ApiError> {
    let q = Query {
        types: vec!["bucket".into()],
        tags_any: vec![TAG.into()],
        lifecycle: vec![Lifecycle::Active, Lifecycle::Archived, Lifecycle::Trashed],
        limit: Some(1),
        ..Query::default()
    };
    Ok(state.store.query_items(&q, Utc::now()).await?.into_iter().next())
}

/// The bucket, the steps and the `contains` edges, validated, as one batch of ops.
pub fn plan(state: &AppState) -> Result<Vec<Op>, ApiError> {
    let now = Utc::now();
    let mut source = CaptureSource::new(SourceKind::Ui);
    source.label = Some(TAG.into());

    let make = |type_name: &str, payload: Value, offset_ms: i64| -> Result<Item, ApiError> {
        let payload: Map<String, Value> = payload.as_object().cloned().unwrap_or_default();
        state
            .plugins
            .validate(type_name, &payload)
            .map_err(|errs| ApiError::new(StatusCode::INTERNAL_SERVER_ERROR, "invalid_onboarding", errs.join("; ")))?;
        let version = state.plugins.get_type(type_name).map(|t| t.manifest.version).unwrap_or(1);
        let mut item = Item::new(type_name, version, source.clone());
        item.payload = payload;
        item.tags = vec![TAG.into()];
        // Distinct timestamps keep the steps in order when sorted by created_at.
        item.created_at = now + Duration::milliseconds(offset_ms);
        item.updated_at = item.created_at;
        recompute(state, &mut item);
        Ok(item)
    };

    let bucket = make(
        "bucket",
        json!({ "name": BUCKET_NAME, "description": "Setup steps for a new Cairn. Tick them off, then archive the checklist." }),
        0,
    )?;
    let mut ops = vec![Op::new(Actor::User, OpKind::ItemCreate { item: bucket.clone() })];
    for (i, (text, notes, priority)) in STEPS.iter().enumerate() {
        let offset = i64::try_from(i).unwrap_or(0) + 1;
        let step = make("todo", json!({ "text": text, "notes": notes, "priority": priority }), offset)?;
        let edge = Edge::new(bucket.id, step.id, EdgeKind::Contains);
        ops.push(Op::new(Actor::User, OpKind::ItemCreate { item: step }));
        ops.push(Op::new(Actor::User, OpKind::EdgeCreate { edge }));
    }
    Ok(ops)
}

/// Seed the checklist unless one exists already.
pub async fn seed(state: &AppState) -> Result<Seeded, ApiError> {
    if let Some(b) = existing(state).await? {
        return Ok(Seeded { created: false, bucket: Some(b.id), steps: 0 });
    }
    let ops = plan(state)?;
    let bucket = ops.iter().find_map(|op| match &op.kind {
        OpKind::ItemCreate { item } if item.item_type == "bucket" => Some(item.id),
        _ => None,
    });
    state.store.apply(ops).await?;
    if let Some(id) = bucket {
        notify_change(state, "item.created", id, "bucket").await;
    }
    Ok(Seeded { created: true, bucket, steps: STEPS.len() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{call, harness};
    use axum::body::Body;
    use axum::http::Request;
    use cairn_store::Store;

    fn post(token: &str) -> Request<Body> {
        Request::post("/api/v1/onboarding")
            .header("authorization", format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap()
    }

    #[tokio::test]
    async fn seeds_valid_steps_in_a_bucket_once() {
        let h = harness();
        let app = crate::api::router().with_state(h.state.clone());

        let (s, body) = call(app.clone(), post("ui")).await;
        assert_eq!(s, StatusCode::CREATED, "{body}");
        assert_eq!(body["created"], true);
        assert_eq!(body["steps"], STEPS.len());

        let todos = h
            .store
            .query_items(
                &Query { types: vec!["todo".into()], tags_any: vec![TAG.into()], ..Query::default() },
                Utc::now(),
            )
            .await
            .unwrap();
        assert_eq!(todos.len(), STEPS.len());
        assert!(todos.iter().all(|t| t.payload.get("done").is_none()), "steps start open");
        assert!(todos.iter().all(|t| !t.title.is_empty()));

        let bucket: Id = body["bucket"].as_str().unwrap().parse().unwrap();
        let edges = h.store.edges_of(bucket).await.unwrap();
        assert_eq!(edges.iter().filter(|e| e.kind == EdgeKind::Contains && e.from == bucket).count(), STEPS.len());
        assert_eq!(h.hub.0.lock().unwrap().len(), 1);

        // Second call writes nothing.
        let (s, body) = call(app, post("ui")).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(body["created"], false);
        assert_eq!(body["bucket"].as_str().unwrap(), bucket.to_string());
        assert_eq!(
            h.store
                .query_items(&Query { tags_any: vec![TAG.into()], ..Query::default() }, Utc::now())
                .await
                .unwrap()
                .len(),
            STEPS.len() + 1
        );
    }

    #[tokio::test]
    async fn trashed_checklist_is_not_reseeded() {
        let h = harness();
        let first = seed(&h.state).await.unwrap();
        let id = first.bucket.unwrap();
        h.store.apply(vec![Op::new(Actor::User, OpKind::ItemLifecycle { id, to: Lifecycle::Trashed })]).await.unwrap();
        let again = seed(&h.state).await.unwrap();
        assert!(!again.created);
    }

    #[tokio::test]
    async fn needs_write_scope() {
        let h = harness();
        let app = crate::api::router().with_state(h.state);
        let (s, _) = call(app, post("cap")).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
    }
}
