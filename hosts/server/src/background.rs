//! The two long-running loops: the spool consumer (replacing the Queue consumers) and the
//! scheduler timer (replacing the Scheduler DO alarm). Neither polls: each sleeps until it's
//! notified or until the next thing is due.

use crate::spool::{Queue, Row, SqliteSpool};
use async_trait::async_trait;
use cairn_app::AppState;
use cairn_app::ports::{CaptureMsg, SchedulerPort};
use cairn_app::schedule::TaskFire;
use cairn_app::{reminders, schedule};
use chrono::{DateTime, Duration, Utc};
use std::sync::Arc;
use tokio::sync::Notify;

const BATCH: usize = 25;
const LAST_TICK: &str = "scheduler.last_tick";

fn until(t: DateTime<Utc>) -> std::time::Duration {
    (t - Utc::now()).to_std().unwrap_or(std::time::Duration::ZERO)
}

enum Outcome {
    Done,
    Retry(String),
    Bury(String),
}

async fn handle(app: &AppState, row: &Row) -> Outcome {
    match row.queue {
        Queue::Captures => match serde_json::from_str::<CaptureMsg>(&row.body) {
            Ok(m) => match cairn_app::ingest::process_capture(app, &m).await {
                Ok(()) => Outcome::Done,
                Err(e) => Outcome::Retry(e.to_string()),
            },
            Err(e) => Outcome::Bury(format!("undecodable capture: {e}")),
        },
        Queue::Tasks => match serde_json::from_str::<TaskFire>(&row.body) {
            Ok(f) => {
                // TODO(M5): resolve input, compute run key, fan out chunks, call gateway, apply.
                tracing::info!(task = %f.task, at = %f.at, "task fired (runner lands in M5)");
                Outcome::Done
            }
            Err(e) => Outcome::Bury(format!("undecodable task fire: {e}")),
        },
    }
}

/// Drain the spool forever. Runs the backlog on startup, so anything accepted before a
/// restart or while SurrealDB was down gets delivered.
pub async fn consume(app: AppState, spool: Arc<SqliteSpool>) {
    loop {
        let rows = match spool.due(Utc::now(), BATCH) {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(error = %e, "spool read failed");
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                continue;
            }
        };
        if rows.is_empty() {
            let wake = spool.next_due().ok().flatten().unwrap_or_else(|| Utc::now() + Duration::hours(1));
            tokio::select! {
                _ = spool.ready.notified() => {}
                _ = tokio::time::sleep(until(wake)) => {}
            }
            continue;
        }
        for row in rows {
            let result = match handle(&app, &row).await {
                Outcome::Done => spool.ack(row.id),
                Outcome::Retry(e) => spool.fail(&row, &e, Utc::now()).map(|dead| {
                    if dead {
                        tracing::error!(id = row.id, queue = ?row.queue, error = %e, "spool row is dead after retries");
                    } else {
                        tracing::warn!(id = row.id, attempt = row.attempts + 1, error = %e, "spool row failed; will retry");
                    }
                }),
                Outcome::Bury(e) => {
                    tracing::error!(id = row.id, error = %e, "spool row buried");
                    spool.bury(&row, &e)
                }
            };
            if let Err(e) = result {
                tracing::error!(error = %e, "spool bookkeeping failed");
            }
        }
    }
}

/// `SchedulerPort` for this process: wake the scheduler loop to recompute its next timer.
pub struct TimerScheduler(pub Arc<Notify>);

#[async_trait]
impl SchedulerPort for TimerScheduler {
    async fn rearm(&self) {
        self.0.notify_one();
    }
}

/// Fire due reminders and enqueue due cron tasks (the old DO `alarm`).
async fn tick(app: &AppState, spool: &SqliteSpool, now: DateTime<Utc>) {
    let last = spool
        .get(LAST_TICK)
        .ok()
        .flatten()
        .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or(now - Duration::hours(1));
    match reminders::fire_due(app, now).await {
        Ok(r) if !r.failed.is_empty() => {
            tracing::warn!(failed = ?r.failed, "some reminders failed; they stay pending and retry next tick")
        }
        Ok(_) => {}
        Err(e) => tracing::error!(error = %e, "firing reminders failed"),
    }
    match schedule::due_cron(app, last, now).await {
        Ok(fires) => {
            for f in fires {
                if let Err(e) = spool.push_task(&f, now) {
                    tracing::error!(error = %e, task = %f.task, "could not enqueue task fire");
                }
            }
        }
        Err(e) => tracing::error!(error = %e, "cron evaluation failed"),
    }
    if let Err(e) = spool.set(LAST_TICK, &now.to_rfc3339()) {
        tracing::error!(error = %e, "could not record scheduler tick");
    }
}

/// Sleep until the earliest pending reminder or cron task (at most an hour, the backstop),
/// tick, repeat. `rearm` wakes it early to recompute. Alarms are at-least-once; run keys and
/// reminder state make an extra tick harmless.
pub async fn schedule(app: AppState, spool: Arc<SqliteSpool>, wake: Arc<Notify>) {
    loop {
        let now = Utc::now();
        let backstop = now + Duration::hours(1);
        let next = match reminders::next_wake(&app, now).await {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(error = %e, "could not compute next wake; using the hourly backstop");
                None
            }
        };
        // Floor at 1s so a due event fires promptly without a hot loop.
        let at = next.map(|t| t.max(now + Duration::seconds(1))).unwrap_or(backstop).min(backstop);
        tokio::select! {
            _ = tokio::time::sleep(until(at)) => tick(&app, &spool, Utc::now()).await,
            _ = wake.notified() => {}
        }
    }
}
