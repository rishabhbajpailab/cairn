//! Ledger Durable Object (SQLite-backed). One instance (`"ledger"`) serializes every
//! reserve/settle, so caps can't be raced by concurrent fan-out.

use crate::caps;
use cairn_gateway_core::ledger::{Ledger, LedgerError, LedgerSql, ReserveRequest, SqlArg};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use worker::{DurableObject, Env, Request, Response, Result, SqlStorage, SqlStorageValue, State, durable_object};

struct DoSql<'a>(&'a SqlStorage);

impl LedgerSql for DoSql<'_> {
    fn exec(&self, sql: &str, args: &[SqlArg]) -> std::result::Result<Vec<Map<String, Value>>, String> {
        let bindings: Vec<SqlStorageValue> = args
            .iter()
            .map(|a| match a {
                SqlArg::Text(s) => SqlStorageValue::String(s.clone()),
                SqlArg::Real(f) => SqlStorageValue::Float(*f),
                SqlArg::Null => SqlStorageValue::Null,
            })
            .collect();
        let cursor = self.0.exec(sql, Some(bindings)).map_err(|e| e.to_string())?;
        cursor.to_array::<Map<String, Value>>().map_err(|e| e.to_string())
    }
}

#[durable_object]
pub struct LedgerDo {
    sql: SqlStorage,
    env: Env,
}

#[derive(Deserialize)]
struct SettleBody {
    id: String,
    actual_usd: Option<f64>,
}

#[derive(Deserialize)]
struct DisableBody {
    reason: String,
}

fn err_response(e: LedgerError) -> Result<Response> {
    let (status, code) = match &e {
        LedgerError::Reject(cairn_gateway_core::ledger::Reject::Disabled(_)) => (503, "disabled"),
        LedgerError::Reject(_) => (402, "budget"),
        LedgerError::Storage(_) => (500, "ledger"),
    };
    let reject = match &e {
        LedgerError::Reject(r) => serde_json::to_value(r).unwrap_or(Value::Null),
        LedgerError::Storage(_) => Value::Null,
    };
    Ok(Response::from_json(&json!({ "error": { "code": code, "message": e.to_string(), "reject": reject } }))?
        .with_status(status))
}

impl DurableObject for LedgerDo {
    fn new(state: State, env: Env) -> Self {
        Self { sql: state.storage().sql(), env }
    }

    async fn fetch(&self, mut req: Request) -> Result<Response> {
        let cfg = caps::config()?;
        let sql = DoSql(&self.sql);
        let ledger = match Ledger::new(&sql, &cfg, caps::enable_after(&self.env)) {
            Ok(l) => l,
            Err(e) => return err_response(e),
        };
        let now = Utc::now();
        let path = req.path();
        let result: std::result::Result<Value, LedgerError> = match path.as_str() {
            "/reserve" => {
                let r: ReserveRequest = req.json().await?;
                ledger.reserve(&r, now).map(|v| json!(v))
            }
            "/settle" => {
                let b: SettleBody = req.json().await?;
                ledger.settle(&b.id, b.actual_usd, now).map(|_| json!({}))
            }
            "/status" => ledger.status(now).map(|s| json!(s)),
            "/disable" => {
                let b: DisableBody = req.json().await?;
                ledger.disable(&b.reason, now).map(|_| json!({ "disabled": true }))
            }
            "/breaker" => ledger.breaker_check(now).map(|r| json!({ "tripped": r })),
            "/key-checked" => ledger.mark_key_checked(now).map(|_| json!({})),
            _ => return Response::error("not found", 404),
        };
        match result {
            Ok(v) => Response::from_json(&v),
            Err(e) => err_response(e),
        }
    }
}
