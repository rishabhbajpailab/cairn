//! Deterministic orchestration (ADR 0004). No LLM decides what runs, when, or on which model.
//!
//! - [`TaskDef`]: typed view of a `task` item's payload (docs/tasks.md)
//! - [`lint`]: static checks run before a task item is saved
//! - [`cron_fires_between`]: cron trigger evaluation in the configured timezone
//! - [`run_key`]: idempotency key so retries never double-charge or duplicate reminders

pub mod lint;
pub mod task;

pub use lint::{LintContext, LintError, lint};
pub use task::{Apply, Budget, Fanout, Select, TaskDef, Trigger};

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use sha2::{Digest, Sha256};
use std::str::FromStr;

/// Convert a standard 5-field cron expression into the 6-field form the `cron` crate wants.
pub fn parse_cron(expr: &str) -> Result<cron::Schedule, cron::error::Error> {
    let fields = expr.split_whitespace().count();
    let six = if fields == 5 { format!("0 {expr}") } else { expr.to_string() };
    cron::Schedule::from_str(&six)
}

/// All fire times in `(from, to]`, evaluated in `tz`. The scheduler tick calls this with the
/// last tick time and now, so a restart catches up on missed firings exactly once
/// (idempotency keys de-duplicate).
pub fn cron_fires_between(
    schedule: &cron::Schedule,
    tz: Tz,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Vec<DateTime<Utc>> {
    schedule
        .after(&from.with_timezone(&tz))
        .take_while(|t| t.with_timezone(&Utc) <= to)
        .map(|t| t.with_timezone(&Utc))
        .collect()
}

/// Next fire time strictly after `after`, evaluated in `tz`. The Scheduler Durable Object uses
/// this to set its single alarm to the earliest upcoming event (no polling).
pub fn next_fire_after(schedule: &cron::Schedule, tz: Tz, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
    schedule.after(&after.with_timezone(&tz)).next().map(|t| t.with_timezone(&Utc))
}

/// `sha256(task_id | task_version | window_start | window_end | sorted input ids)`.
pub fn run_key(
    task_id: &str,
    task_version: u64,
    window: (DateTime<Utc>, DateTime<Utc>),
    input_ids: &[String],
) -> String {
    let mut ids = input_ids.to_vec();
    ids.sort();
    let mut h = Sha256::new();
    h.update(task_id.as_bytes());
    h.update(b"|");
    h.update(task_version.to_le_bytes());
    h.update(b"|");
    h.update(window.0.to_rfc3339().as_bytes());
    h.update(b"|");
    h.update(window.1.to_rfc3339().as_bytes());
    for id in ids {
        h.update(b"|");
        h.update(id.as_bytes());
    }
    hex::encode(h.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn nightly_cron_in_local_time() {
        let s = parse_cron("0 21 * * *").unwrap();
        let tz: Tz = "America/Los_Angeles".parse().unwrap();
        // 21:00 PST on 2026-11-23 = 05:00Z on 11-24
        let fires = cron_fires_between(&s, tz, t("2026-11-23T12:00:00Z"), t("2026-11-24T12:00:00Z"));
        assert_eq!(fires, vec![t("2026-11-24T05:00:00Z")]);
    }

    #[test]
    fn next_fire_is_strictly_after() {
        let s = parse_cron("0 21 * * *").unwrap();
        let tz: Tz = "America/Los_Angeles".parse().unwrap();
        let at = t("2026-11-24T05:00:00Z");
        assert_eq!(next_fire_after(&s, tz, at), Some(t("2026-11-25T05:00:00Z")));
    }

    #[test]
    fn run_key_is_order_independent() {
        let w = (t("2026-11-23T00:00:00Z"), t("2026-11-24T00:00:00Z"));
        let a = run_key("T", 1, w, &["b".into(), "a".into()]);
        let b = run_key("T", 1, w, &["a".into(), "b".into()]);
        assert_eq!(a, b);
        assert_ne!(a, run_key("T", 2, w, &["a".into(), "b".into()]));
    }
}
