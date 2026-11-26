//! Scheduler Durable Object: one alarm, set to the earliest upcoming event (pending reminder
//! or cron task). No polling. Alarms are at-least-once; run keys and reminder state make
//! double firing harmless.

use crate::state;
use cairn_app::{reminders, schedule};
use chrono::{DateTime, Duration, Utc};
use worker::{DurableObject, Env, Request, Response, Result, State, durable_object};

const LAST_TICK: &str = "last_tick";

#[durable_object]
pub struct Scheduler {
    state: State,
    env: Env,
}

impl Scheduler {
    async fn rearm(&self, app: &cairn_app::AppState, now: DateTime<Utc>) -> Result<Option<DateTime<Utc>>> {
        let next = reminders::next_wake(app, now).await.map_err(|e| worker::Error::RustError(e.to_string()))?;
        match next {
            // Floor at 1s in the future so a due event fires promptly without a hot loop.
            Some(t) => self.state.storage().set_alarm(t.max(now + Duration::seconds(1)).timestamp_millis()).await?,
            None => self.state.storage().delete_alarm().await?,
        }
        Ok(next)
    }
}

impl DurableObject for Scheduler {
    fn new(state: State, env: Env) -> Self {
        Self { state, env }
    }

    /// `POST /rearm`: called after any write to a reminder or task item, and by the hourly
    /// cron backstop.
    async fn fetch(&self, _req: Request) -> Result<Response> {
        let app = state::build(&self.env, true)?;
        let next = self.rearm(&app, Utc::now()).await?;
        Response::from_json(&serde_json::json!({ "next_alarm": next }))
    }

    async fn alarm(&self) -> Result<Response> {
        let now = Utc::now();
        let app = state::build(&self.env, true)?;
        let last: DateTime<Utc> = self
            .state
            .storage()
            .get::<String>(LAST_TICK)
            .await
            .ok()
            .flatten()
            .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
            .map(|t| t.with_timezone(&Utc))
            .unwrap_or(now - Duration::hours(1));

        let report = reminders::fire_due(&app, now).await.map_err(|e| worker::Error::RustError(e.to_string()))?;
        if !report.failed.is_empty() {
            tracing::warn!(failed = ?report.failed, "some reminders failed; they stay pending and retry next alarm");
        }

        let fires = schedule::due_cron(&app, last, now).await.map_err(|e| worker::Error::RustError(e.to_string()))?;
        if !fires.is_empty() {
            let q = self.env.queue("TASKS")?;
            for f in fires {
                q.send(f).await?;
            }
        }

        self.state.storage().put(LAST_TICK, now.to_rfc3339()).await?;
        self.rearm(&app, now).await?;
        Response::ok("ok")
    }
}
