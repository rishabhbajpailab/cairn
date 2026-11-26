//! Task triggers, evaluated by the Scheduler Durable Object when its alarm fires.

use crate::state::AppState;
use cairn_domain::{Cmp, Id, Query, Where};
use cairn_scheduler::{TaskDef, Trigger};
use cairn_store::StoreError;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub struct TaskItem {
    pub id: Id,
    pub def: TaskDef,
}

/// One task firing, sent to the TASKS queue. The consumer computes the run key, so duplicate
/// deliveries (at-least-once alarms and queues) collapse into one run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskFire {
    pub task: Id,
    pub at: DateTime<Utc>,
}

pub async fn enabled_tasks(state: &AppState, now: DateTime<Utc>) -> Result<Vec<TaskItem>, StoreError> {
    let q = Query {
        types: vec!["task".into()],
        filters: vec![Where { field: "enabled".into(), op: Cmp::Eq, value: json!(true) }],
        limit: Some(500),
        ..Default::default()
    };
    Ok(state
        .store
        .query_items(&q, now)
        .await?
        .into_iter()
        .filter_map(|it| {
            let def: TaskDef = serde_json::from_value(Value::Object(it.payload.clone())).ok()?;
            Some(TaskItem { id: it.id, def })
        })
        .collect())
}

/// Cron firings in `(from, to]` for enabled tasks.
pub async fn due_cron(state: &AppState, from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Vec<TaskFire>, StoreError> {
    let mut out = Vec::new();
    for t in enabled_tasks(state, to).await? {
        if let Trigger::Cron(expr) = &t.def.trigger
            && let Ok(s) = cairn_scheduler::parse_cron(expr)
        {
            for at in cairn_scheduler::cron_fires_between(&s, state.cfg.tz(), from, to) {
                out.push(TaskFire { task: t.id, at });
            }
        }
    }
    Ok(out)
}
