//! Garbage collection planning (docs/lifecycle-gc.md).
//!
//! [`plan`] is a pure function: given a snapshot of items and edges, the policies, and `now`,
//! it returns the ops GC *would* apply plus a report. The `builtin/gc` task applies the ops
//! (as `Actor::Gc`) or, in dry-run, only returns the report. No I/O here.

use cairn_domain::{Actor, Edge, EdgeKind, Id, Item, Lifecycle, Op, OpKind, Where, reltime};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GcConfig {
    #[serde(default = "default_retention")]
    pub trash_retention: String,
    /// Resolved raw captures (the `capture` table, not items) are deleted after this.
    #[serde(default = "default_capture_retention")]
    pub capture_retention: String,
    #[serde(default, rename = "policy")]
    pub policies: Vec<Policy>,
}

fn default_retention() -> String {
    "30d".into()
}

fn default_capture_retention() -> String {
    "7d".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Policy {
    #[serde(rename = "match")]
    pub matcher: Match,
    /// Measured from `updated_at`.
    pub older_than: String,
    pub action: Action,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Match {
    pub types: Vec<String>,
    pub tags_none: Vec<String>,
    #[serde(rename = "where")]
    pub filters: Vec<Where>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Archive,
    Trash,
    /// No state change; surfaces the item in the next digest with Keep/Archive/Trash buttons.
    Nudge,
}

/// Types GC never touches automatically, regardless of policy.
pub const NEVER_GC_TYPES: &[&str] = &["task", "bucket"];

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub struct Report {
    pub archived: Vec<Id>,
    pub trashed: Vec<Id>,
    pub purged: Vec<Id>,
    pub nudged: Vec<Id>,
    /// Trashed items past retention that still have inbound references from active items.
    pub skipped_referenced: Vec<Id>,
}

pub struct Plan {
    pub ops: Vec<Op>,
    pub report: Report,
}

pub fn plan(items: &[Item], edges: &[Edge], cfg: &GcConfig, now: DateTime<Utc>) -> Plan {
    let mut report = Report::default();
    let mut ops = Vec::new();
    let by_id: HashMap<Id, &Item> = items.iter().map(|i| (i.id, i)).collect();
    let mut touched: HashSet<Id> = HashSet::new();

    // 1. Purge trashed items past retention (only ever trashed items).
    let retention = reltime::parse_span(&cfg.trash_retention).unwrap_or(chrono::Duration::days(30));
    for it in items.iter().filter(|i| i.lifecycle == Lifecycle::Trashed) {
        if it.is_protected() || NEVER_GC_TYPES.contains(&it.item_type.as_str()) {
            continue;
        }
        let Some(trashed_at) = it.trashed_at else { continue };
        if now - trashed_at < retention {
            continue;
        }
        let referenced = edges.iter().any(|e| {
            e.to == it.id
                && matches!(e.kind, EdgeKind::Contains | EdgeKind::RemindAbout)
                && by_id.get(&e.from).is_some_and(|src| src.lifecycle == Lifecycle::Active)
        });
        if referenced {
            report.skipped_referenced.push(it.id);
            continue;
        }
        ops.push(Op::new(Actor::Gc, OpKind::ItemPurge { id: it.id }));
        report.purged.push(it.id);
        touched.insert(it.id);
    }

    // 2. Policies on active items (first matching policy wins, in config order).
    for it in items.iter().filter(|i| i.lifecycle == Lifecycle::Active) {
        if it.is_protected() || NEVER_GC_TYPES.contains(&it.item_type.as_str()) || touched.contains(&it.id) {
            continue;
        }
        let Some(policy) = cfg.policies.iter().find(|p| matches(it, &p.matcher)) else { continue };
        let Ok(age) = reltime::parse_span(&policy.older_than) else { continue };
        if now - it.updated_at < age {
            continue;
        }
        match policy.action {
            Action::Archive => {
                ops.push(Op::new(Actor::Gc, OpKind::ItemLifecycle { id: it.id, to: Lifecycle::Archived }));
                report.archived.push(it.id);
            }
            Action::Trash => {
                ops.push(Op::new(Actor::Gc, OpKind::ItemLifecycle { id: it.id, to: Lifecycle::Trashed }));
                report.trashed.push(it.id);
            }
            Action::Nudge => report.nudged.push(it.id),
        }
    }

    Plan { ops, report }
}

fn matches(it: &Item, m: &Match) -> bool {
    if !m.types.is_empty() && !m.types.contains(&it.item_type) {
        return false;
    }
    if m.tags_none.iter().any(|t| it.tags.contains(t)) {
        return false;
    }
    m.filters.iter().all(|w| {
        let v = it.idx.get(&w.field).or_else(|| it.payload.get(&w.field));
        match w.op {
            cairn_domain::Cmp::Eq => v == Some(&w.value),
            cairn_domain::Cmp::Ne => v != Some(&w.value),
            cairn_domain::Cmp::Exists => v.is_some(),
            // Policies intentionally support only eq/ne/exists: keep GC rules obvious.
            _ => false,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cairn_domain::{CaptureSource, SourceKind};
    use chrono::Duration;
    use serde_json::json;

    fn cfg() -> GcConfig {
        serde_json::from_value(json!({
            "trash_retention": "30d",
            "policy": [
                { "match": { "types": ["todo"], "where": [{ "field": "done", "op": "eq", "value": true }] },
                  "older_than": "14d", "action": "archive" },
                { "match": { "types": ["hold-that-thought"], "tags_none": ["keep"] },
                  "older_than": "60d", "action": "nudge" }
            ]
        }))
        .unwrap()
    }

    fn item(t: &str, age_days: i64, now: DateTime<Utc>) -> Item {
        let mut i = Item::new(t, 1, CaptureSource::new(SourceKind::Ui));
        i.updated_at = now - Duration::days(age_days);
        i
    }

    #[test]
    fn archives_old_done_todos_but_not_pinned() {
        let now = Utc::now();
        let mut done = item("todo", 20, now);
        done.idx.insert("done".into(), json!(true));
        let mut pinned = done.clone();
        pinned.id = Id::new();
        pinned.pinned = true;
        let fresh = {
            let mut f = item("todo", 2, now);
            f.idx.insert("done".into(), json!(true));
            f
        };
        let p = plan(&[done.clone(), pinned, fresh], &[], &cfg(), now);
        assert_eq!(p.report.archived, vec![done.id]);
    }

    #[test]
    fn purges_only_expired_unreferenced_trash() {
        let now = Utc::now();
        let mut old = item("todo", 40, now);
        old.lifecycle = Lifecycle::Trashed;
        old.trashed_at = Some(now - Duration::days(31));
        let mut referenced = old.clone();
        referenced.id = Id::new();
        let bucket = item("bucket", 1, now);
        let edge = Edge::new(bucket.id, referenced.id, EdgeKind::Contains);
        let mut recent = old.clone();
        recent.id = Id::new();
        recent.trashed_at = Some(now - Duration::days(3));

        let p = plan(&[old.clone(), referenced.clone(), bucket, recent], &[edge], &cfg(), now);
        assert_eq!(p.report.purged, vec![old.id]);
        assert_eq!(p.report.skipped_referenced, vec![referenced.id]);
        assert!(p.ops.iter().all(|o| o.actor == Actor::Gc));
    }

    #[test]
    fn never_touches_tasks_or_buckets() {
        let now = Utc::now();
        let mut t = item("task", 400, now);
        t.lifecycle = Lifecycle::Trashed;
        t.trashed_at = Some(now - Duration::days(400));
        assert!(plan(&[t], &[], &cfg(), now).ops.is_empty());
    }
}
