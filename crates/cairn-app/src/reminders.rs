//! Deterministic reminder delivery and next-wake computation for the Scheduler Durable
//! Object. No polling: the DO sets one alarm at `next_wake`, fires what's due, re-arms.

use crate::items::patch_payload;
use crate::state::AppState;
use cairn_domain::{Actor, Cmp, Item, Query, Where};
use cairn_store::StoreError;
use chrono::{DateTime, NaiveTime, TimeZone, Utc};
use serde_json::{Map, Value, json};

#[derive(Debug, Default, PartialEq)]
pub struct FireReport {
    pub sent: usize,
    pub deferred_quiet: usize,
    pub failed: Vec<String>,
}

fn parse_time(v: Option<&Value>) -> Option<DateTime<Utc>> {
    v?.as_str().and_then(|s| DateTime::parse_from_rfc3339(s).ok()).map(|t| t.with_timezone(&Utc))
}

/// When a reminder should fire: max(fire_at, snoozed_until).
pub fn effective_fire_at(item: &Item) -> Option<DateTime<Utc>> {
    let fire = parse_time(item.payload.get("fire_at"))?;
    Some(parse_time(item.payload.get("snoozed_until")).map_or(fire, |s| s.max(fire)))
}

/// If `t` falls inside quiet hours, the time the window ends; otherwise `None`.
pub fn quiet_until(state: &AppState, t: DateTime<Utc>) -> Option<DateTime<Utc>> {
    let [start, end] = state.cfg.notify.quiet_hours.as_ref()?;
    let (s, e) = (NaiveTime::parse_from_str(start, "%H:%M").ok()?, NaiveTime::parse_from_str(end, "%H:%M").ok()?);
    let tz = state.cfg.tz();
    let local = t.with_timezone(&tz);
    let now_t = local.time();
    let inside = if s <= e { now_t >= s && now_t < e } else { now_t >= s || now_t < e };
    if !inside {
        return None;
    }
    let mut day = local.date_naive();
    if s > e && now_t >= s {
        day = day.succ_opt()?; // window wraps midnight: ends tomorrow
    }
    tz.from_local_datetime(&day.and_time(e)).earliest().map(|d| d.with_timezone(&Utc))
}

async fn pending(state: &AppState, now: DateTime<Utc>) -> Result<Vec<Item>, StoreError> {
    let q = Query {
        types: vec!["reminder".into()],
        filters: vec![Where { field: "state".into(), op: Cmp::Eq, value: json!("pending") }],
        limit: Some(500),
        ..Default::default()
    };
    state.store.query_items(&q, now).await
}

/// Send every due reminder (outside quiet hours) and mark it `sent`.
pub async fn fire_due(state: &AppState, now: DateTime<Utc>) -> Result<FireReport, StoreError> {
    let mut report = FireReport::default();
    let Some(keys) = state.discord.as_ref() else { return Ok(report) };
    for r in pending(state, now).await? {
        let Some(at) = effective_fire_at(&r) else { continue };
        if at > now {
            continue;
        }
        if quiet_until(state, now).is_some() {
            report.deferred_quiet += 1;
            continue;
        }
        let buttons: Vec<(String, String)> = state
            .plugins
            .get_type("reminder")
            .map(|t| {
                t.manifest
                    .actions
                    .iter()
                    .filter_map(|(name, a)| a.discord_button.clone().map(|l| (l, format!("act:{}:{name}", r.id))))
                    .collect()
            })
            .unwrap_or_default();
        let text = format!("⏰ {}", r.payload.get("message").and_then(Value::as_str).unwrap_or(&r.title));
        match crate::discord::send_dm(state.http.as_ref(), keys, &text, &buttons).await {
            Ok(()) => {
                let mut p = Map::new();
                p.insert("state".into(), json!("sent"));
                if let Err(e) = patch_payload(state, Actor::User, r.id, p).await {
                    report.failed.push(format!("{}: sent but not marked: {}", r.id, e.message));
                } else {
                    report.sent += 1;
                }
            }
            Err(e) => report.failed.push(format!("{}: {e}", r.id)),
        }
    }
    Ok(report)
}

/// Earliest future moment anything needs doing: a pending reminder (pushed past quiet hours)
/// or an enabled cron task. The Scheduler DO sets its single alarm to this.
pub async fn next_wake(state: &AppState, now: DateTime<Utc>) -> Result<Option<DateTime<Utc>>, StoreError> {
    let mut next: Option<DateTime<Utc>> = None;
    let mut consider = |t: DateTime<Utc>| next = Some(next.map_or(t, |n| n.min(t)));
    for r in pending(state, now).await? {
        if let Some(at) = effective_fire_at(&r) {
            let at = at.max(now);
            consider(quiet_until(state, at).unwrap_or(at));
        }
    }
    for t in crate::schedule::enabled_tasks(state, now).await? {
        if let cairn_scheduler::Trigger::Cron(expr) = &t.def.trigger
            && let Ok(s) = cairn_scheduler::parse_cron(expr)
            && let Some(at) = cairn_scheduler::next_fire_after(&s, state.cfg.tz(), now)
        {
            consider(at);
        }
    }
    Ok(next)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discord::DiscordKeys;
    use crate::testing::harness_with;
    use cairn_domain::{CaptureSource, Op, OpKind, SourceKind};
    use cairn_http::FakeTransport;
    use chrono::Duration;

    fn reminder(fire_at: DateTime<Utc>) -> Item {
        let mut it = Item::new("reminder", 1, CaptureSource::new(SourceKind::Task));
        it.payload.insert("message".into(), json!("renew registration"));
        it.payload.insert("fire_at".into(), json!(fire_at.to_rfc3339()));
        it.payload.insert("state".into(), json!("pending"));
        it.idx.insert("state".into(), json!("pending"));
        it
    }

    async fn seed(h: &crate::testing::Harness, items: Vec<Item>) {
        let ops = items.into_iter().map(|item| Op::new(Actor::User, OpKind::ItemCreate { item })).collect();
        h.state.store.apply(ops).await.unwrap();
    }

    fn with_discord(mut h: crate::testing::Harness) -> crate::testing::Harness {
        let sk = ed25519_dalek::SigningKey::from_bytes(&[1u8; 32]);
        let pk = hex::encode(sk.verifying_key().to_bytes());
        h.state.discord = Some(DiscordKeys::new(&pk, "bot".into(), "123").unwrap());
        h
    }

    #[tokio::test]
    async fn fires_due_reminders_once_and_marks_sent() {
        let http = FakeTransport::with_responses(vec![
            FakeTransport::json(200, json!({ "id": "dm1" })),
            FakeTransport::json(200, json!({ "id": "msg1" })),
        ]);
        let h = with_discord(harness_with("timezone = \"UTC\"", http));
        let now = Utc::now();
        let due = reminder(now - Duration::minutes(1));
        let later = reminder(now + Duration::hours(2));
        seed(&h, vec![due.clone(), later.clone()]).await;

        let r = fire_due(&h.state, now).await.unwrap();
        assert_eq!(r, FireReport { sent: 1, deferred_quiet: 0, failed: vec![] });
        let msg = h.http.sent()[1].body_json().unwrap();
        assert!(msg["content"].as_str().unwrap().contains("renew registration"));
        assert!(!msg["components"][0]["components"].as_array().unwrap().is_empty(), "action buttons attached");

        // already sent: a second alarm sends nothing (no canned responses left would error)
        assert_eq!(fire_due(&h.state, now).await.unwrap().sent, 0);
        assert_eq!(next_wake(&h.state, now).await.unwrap(), effective_fire_at(&later));
    }

    #[tokio::test]
    async fn quiet_hours_defer_and_push_next_wake() {
        let cfg = "timezone = \"UTC\"\n[notify]\nquiet_hours = [\"22:00\", \"07:00\"]";
        let h = with_discord(harness_with(cfg, FakeTransport::default()));
        let now = DateTime::parse_from_rfc3339("2026-11-23T23:30:00Z").unwrap().with_timezone(&Utc);
        seed(&h, vec![reminder(now - Duration::minutes(5))]).await;
        let r = fire_due(&h.state, now).await.unwrap();
        assert_eq!(r.deferred_quiet, 1);
        assert_eq!(next_wake(&h.state, now).await.unwrap().unwrap().to_rfc3339(), "2026-11-24T07:00:00+00:00");
    }
}
