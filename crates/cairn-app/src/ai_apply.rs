//! AI output → proposed ops (ADR 0007). Each converter:
//! 1. receives output that already validated against the capability's output schema,
//! 2. maps it to ops drawn ONLY from its allowed op kinds,
//! 3. validates references and invariants (targets exist, fire_at in the future, caps).
//!
//! Model output is never applied directly, and converters are hand-written Rust.

use cairn_domain::{Actor, Id, Item, Op, OpKind};
use chrono::{DateTime, Duration, Utc};
use serde_json::{Map, Value};
use std::collections::HashSet;

#[derive(Debug, Default)]
pub struct Proposal {
    pub ops: Vec<Op>,
    /// Output entries dropped by validation, with reasons. Shown on the task-run item.
    pub rejected: Vec<String>,
}

pub struct ApplyCtx<'a> {
    pub actor: Actor,
    pub now: DateTime<Utc>,
    /// Items the run was given as input: converters may only reference these.
    pub input_ids: &'a HashSet<Id>,
    pub known_types: &'a HashSet<String>,
    pub max_reminders: u32,
}

/// Allowed op kinds per converter: a converter emitting anything else is a bug.
pub fn allowed(converter: &str) -> &'static [&'static str] {
    match converter {
        "triage" => &["item_patch"],
        "digest" => &["item_create", "edge_create"],
        _ => &[],
    }
}

fn op_kind_name(k: &OpKind) -> &'static str {
    match k {
        OpKind::ItemCreate { .. } => "item_create",
        OpKind::ItemPatch { .. } => "item_patch",
        OpKind::ItemLifecycle { .. } => "item_lifecycle",
        OpKind::ItemPurge { .. } => "item_purge",
        OpKind::EdgeCreate { .. } => "edge_create",
        OpKind::EdgeDelete { .. } => "edge_delete",
        OpKind::ViewCreate { .. } => "view_create",
        OpKind::ViewLayout { .. } => "view_layout",
    }
}

pub fn convert(converter: &str, output: &Value, ctx: &ApplyCtx<'_>) -> Proposal {
    let mut p = match converter {
        "digest" => digest(output, ctx),
        "triage" => triage(output, ctx),
        other => Proposal { ops: vec![], rejected: vec![format!("unknown converter `{other}`")] },
    };
    // Defense in depth: drop anything outside the whitelist.
    let allow = allowed(converter);
    let before = p.ops.len();
    p.ops.retain(|op| allow.contains(&op_kind_name(&op.kind)));
    if p.ops.len() != before {
        p.rejected.push(format!("{} ops outside `{converter}` whitelist dropped", before - p.ops.len()));
    }
    p
}

fn parse_id(v: &Value) -> Option<Id> {
    v.as_str()?.parse().ok()
}

/// triage: tags (and, later, type changes / bucket membership) for input items only.
fn triage(output: &Value, ctx: &ApplyCtx<'_>) -> Proposal {
    let mut p = Proposal::default();
    for r in output["results"].as_array().into_iter().flatten() {
        let Some(id) = parse_id(&r["id"]) else {
            p.rejected.push("result with invalid id".into());
            continue;
        };
        if !ctx.input_ids.contains(&id) {
            p.rejected.push(format!("{id}: not in run input"));
            continue;
        }
        let tags: Vec<String> = r["tags"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(cairn_domain::normalize_tag)
            .collect();
        // TODO(M5): type change + bucket `contains` edge (needs payload re-validation).
        p.ops.push(Op::new(
            ctx.actor.clone(),
            OpKind::ItemPatch { id, title: None, payload: None, tags: Some(tags), pinned: None, idx: None },
        ));
    }
    p
}

/// digest: one digest item (summary + follow-ups) and validated reminders.
fn digest(output: &Value, ctx: &ApplyCtx<'_>) -> Proposal {
    let mut p = Proposal::default();
    let horizon = ctx.now + Duration::hours(48);
    let mut count = 0u32;
    for r in output["reminders"].as_array().into_iter().flatten() {
        let Some(target) = parse_id(&r["item_id"]) else {
            p.rejected.push("reminder with invalid item_id".into());
            continue;
        };
        if !ctx.input_ids.contains(&target) {
            p.rejected.push(format!("reminder for {target}: not in run input"));
            continue;
        }
        let Some(fire_at) = r["fire_at"].as_str().and_then(|s| DateTime::parse_from_rfc3339(s).ok()) else {
            p.rejected.push(format!("reminder for {target}: unparseable fire_at"));
            continue;
        };
        let fire_at = fire_at.with_timezone(&Utc);
        if fire_at <= ctx.now || fire_at > horizon {
            p.rejected.push(format!("reminder for {target}: fire_at {fire_at} outside (now, now+48h]"));
            continue;
        }
        if count >= ctx.max_reminders {
            p.rejected.push(format!("reminder for {target}: daily reminder cap reached"));
            continue;
        }
        count += 1;
        let mut payload = Map::new();
        payload.insert("message".into(), r["message"].clone());
        payload.insert("fire_at".into(), Value::String(fire_at.to_rfc3339()));
        payload.insert("state".into(), Value::String("pending".into()));
        let mut item = Item::new("reminder", 1, cairn_domain::CaptureSource::new(cairn_domain::SourceKind::Task));
        item.title = r["message"].as_str().unwrap_or("Reminder").chars().take(120).collect();
        item.idx.insert("fire_at".into(), Value::String(fire_at.to_rfc3339()));
        item.idx.insert("state".into(), Value::String("pending".into()));
        item.payload = payload;
        let edge = cairn_domain::Edge::new(item.id, target, cairn_domain::EdgeKind::RemindAbout);
        p.ops.push(Op::new(ctx.actor.clone(), OpKind::ItemCreate { item }));
        p.ops.push(Op::new(ctx.actor.clone(), OpKind::EdgeCreate { edge }));
    }
    // TODO(M5): create the `digest` item from summary/followups once a digest type plugin exists.
    let _ = ctx.known_types;
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn digest_rejects_invented_targets_and_bad_dates() {
        let now = Utc::now();
        let real = Id::new();
        let inputs: HashSet<Id> = [real].into();
        let types: HashSet<String> = ["reminder".to_string()].into();
        let ctx = ApplyCtx { actor: Actor::User, now, input_ids: &inputs, known_types: &types, max_reminders: 1 };
        let soon = (now + Duration::hours(3)).to_rfc3339();
        let out = json!({
            "summary": ["x"], "followups": [],
            "reminders": [
                { "item_id": real.to_string(), "fire_at": soon, "message": "renew registration" },
                { "item_id": Id::new().to_string(), "fire_at": soon, "message": "invented item" },
                { "item_id": real.to_string(), "fire_at": "2020-01-01T00:00:00Z", "message": "past" },
                { "item_id": real.to_string(), "fire_at": soon, "message": "over cap" }
            ]
        });
        let p = convert("digest", &out, &ctx);
        assert_eq!(p.ops.len(), 2, "one reminder item + one remind-about edge");
        assert_eq!(p.rejected.len(), 3);
    }
}
