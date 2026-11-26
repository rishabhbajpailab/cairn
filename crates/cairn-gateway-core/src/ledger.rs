//! Spend ledger. In production it lives in the Ledger Durable Object's SQLite storage, which
//! only the gateway Worker can reach. A Durable Object processes one request at a time and
//! nothing here awaits between the cap check and the insert, so `reserve` is atomic without
//! explicit transactions (DO SQLite forbids `BEGIN`).
//!
//! Every request is `reserve(worst_case)` → call → `settle(actual)`. Unsettled reservations
//! stay counted at their worst case until settled.

use crate::config::GatewayConfig;
use chrono::{DateTime, Datelike, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Minimal SQL surface, implemented by the Durable Object's `SqlStorage` in the Worker and by
/// rusqlite in tests.
pub enum SqlArg {
    Text(String),
    Real(f64),
    Null,
}

pub trait LedgerSql {
    fn exec(&self, sql: &str, args: &[SqlArg]) -> Result<Vec<Map<String, Value>>, String>;
}

#[derive(Debug, thiserror::Error, PartialEq, Clone, Serialize, Deserialize)]
pub enum Reject {
    #[error("gateway is disabled: {0}")]
    Disabled(String),
    #[error("model `{0}` is not on the allowlist")]
    ModelNotAllowed(String),
    #[error("request worst case ${worst:.4} exceeds per-request limit ${limit:.4}")]
    OverRequest { worst: f64, limit: f64 },
    #[error("{scope} cap reached: ${spent:.4} spent + ${worst:.4} > ${cap:.4}")]
    OverCap { scope: String, spent: f64, worst: f64, cap: f64 },
}

#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    #[error(transparent)]
    Reject(#[from] Reject),
    #[error("ledger storage: {0}")]
    Storage(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reservation {
    pub id: String,
    pub worst_usd: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReserveRequest {
    pub task: String,
    pub plugin: String,
    pub run_key: String,
    pub model: String,
    pub worst_usd: f64,
    /// From the task definition (core-supplied, so advisory). Global/plugin caps are authoritative.
    pub task_per_run_usd: f64,
    pub task_per_day_usd: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct LedgerStatus {
    pub disabled: Option<String>,
    pub spent_today_usd: f64,
    pub spent_last_hour_usd: f64,
    pub rejections_last_10m: u32,
    pub key_checked_at: Option<DateTime<Utc>>,
}

const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS reservation (
        id TEXT PRIMARY KEY, task TEXT NOT NULL, plugin TEXT NOT NULL, run_key TEXT NOT NULL,
        model TEXT NOT NULL, worst_usd REAL NOT NULL, actual_usd REAL,
        state TEXT NOT NULL CHECK (state IN ('reserved','settled','released')),
        created_at TEXT NOT NULL, settled_at TEXT)",
    "CREATE INDEX IF NOT EXISTS reservation_created ON reservation(created_at)",
    "CREATE TABLE IF NOT EXISTS rejection (at TEXT NOT NULL, reason TEXT NOT NULL)",
    "CREATE TABLE IF NOT EXISTS flag (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
];

/// Counted against caps: actual if settled, worst case if still reserved, 0 if released.
const CHARGED: &str =
    "COALESCE(SUM(CASE state WHEN 'settled' THEN actual_usd WHEN 'reserved' THEN worst_usd ELSE 0 END), 0) AS v";

pub struct Ledger<'a, S: LedgerSql> {
    sql: &'a S,
    cfg: &'a GatewayConfig,
    /// Deploy-time `ENABLE_AFTER` var: a disable recorded before this instant no longer applies.
    /// This is the ONLY way back on, and it requires a deploy.
    enable_after: Option<DateTime<Utc>>,
}

fn ts(t: DateTime<Utc>) -> SqlArg {
    SqlArg::Text(t.to_rfc3339())
}

/// Caps use UTC days/months to line up with OpenRouter's `usage_daily` / `usage_monthly`.
fn day_start(now: DateTime<Utc>) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(now.year(), now.month(), now.day(), 0, 0, 0).single().unwrap_or(now)
}

fn month_start(now: DateTime<Utc>) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(now.year(), now.month(), 1, 0, 0, 0).single().unwrap_or(now)
}

impl<'a, S: LedgerSql> Ledger<'a, S> {
    pub fn new(sql: &'a S, cfg: &'a GatewayConfig, enable_after: Option<DateTime<Utc>>) -> Result<Self, LedgerError> {
        for stmt in SCHEMA {
            sql.exec(stmt, &[]).map_err(LedgerError::Storage)?;
        }
        Ok(Self { sql, cfg, enable_after })
    }

    fn sum(&self, where_sql: &str, args: &[SqlArg]) -> Result<f64, LedgerError> {
        let rows = self
            .sql
            .exec(&format!("SELECT {CHARGED} FROM reservation WHERE {where_sql}"), args)
            .map_err(LedgerError::Storage)?;
        Ok(rows.first().and_then(|r| r.get("v")).and_then(Value::as_f64).unwrap_or(0.0))
    }

    fn flag(&self, key: &str) -> Result<Option<String>, LedgerError> {
        let rows = self
            .sql
            .exec("SELECT value FROM flag WHERE key = ?", &[SqlArg::Text(key.into())])
            .map_err(LedgerError::Storage)?;
        Ok(rows.first().and_then(|r| r.get("value")).and_then(Value::as_str).map(str::to_owned))
    }

    fn set_flag(&self, key: &str, value: &str) -> Result<(), LedgerError> {
        self.sql
            .exec(
                "INSERT INTO flag (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                &[SqlArg::Text(key.into()), SqlArg::Text(value.into())],
            )
            .map(|_| ())
            .map_err(LedgerError::Storage)
    }

    /// `Some(reason)` if disabled. A disable recorded before the deploy-time `enable_after`
    /// is considered lifted.
    pub fn disabled(&self) -> Result<Option<String>, LedgerError> {
        let Some(raw) = self.flag("disabled")? else { return Ok(None) };
        let (at, reason) = raw.split_once(' ').unwrap_or((raw.as_str(), ""));
        let at = DateTime::parse_from_rfc3339(at).map(|t| t.with_timezone(&Utc)).ok();
        match (at, self.enable_after) {
            (Some(at), Some(after)) if after > at => Ok(None),
            _ => Ok(Some(reason.to_string())),
        }
    }

    /// Tighten-only. Always records the latest disable time, so an old ENABLE_AFTER can't
    /// lift a newer disable.
    pub fn disable(&self, reason: &str, now: DateTime<Utc>) -> Result<(), LedgerError> {
        tracing::warn!(reason, "gateway disabling itself");
        self.set_flag("disabled", &format!("{} {reason}", now.to_rfc3339()))
    }

    pub fn key_checked_at(&self) -> Result<Option<DateTime<Utc>>, LedgerError> {
        Ok(self
            .flag("key_checked_at")?
            .and_then(|s| DateTime::parse_from_rfc3339(&s).ok())
            .map(|t| t.with_timezone(&Utc)))
    }

    pub fn mark_key_checked(&self, now: DateTime<Utc>) -> Result<(), LedgerError> {
        self.set_flag("key_checked_at", &now.to_rfc3339())
    }

    pub fn reserve(&self, req: &ReserveRequest, now: DateTime<Utc>) -> Result<Reservation, LedgerError> {
        if let Some(reason) = self.disabled()? {
            return Err(Reject::Disabled(reason).into());
        }
        if self.cfg.allowed(&req.model).is_none() {
            return Err(self.reject(Reject::ModelNotAllowed(req.model.clone()), now));
        }
        let limit = self.cfg.global.per_request_max_usd.min(req.task_per_run_usd);
        if req.worst_usd > limit {
            return Err(self.reject(Reject::OverRequest { worst: req.worst_usd, limit }, now));
        }

        let day = day_start(now);
        let month = month_start(now);
        let mut checks: Vec<(String, f64, f64)> = vec![
            ("global daily".into(), self.sum("created_at >= ?", &[ts(day)])?, self.cfg.global.per_day_usd),
            ("global monthly".into(), self.sum("created_at >= ?", &[ts(month)])?, self.cfg.global.per_month_usd),
            (
                format!("task `{}` daily", req.task),
                self.sum("created_at >= ? AND task = ?", &[ts(day), SqlArg::Text(req.task.clone())])?,
                req.task_per_day_usd,
            ),
        ];
        if let Some(cap) = self.cfg.plugins.get(&req.plugin) {
            checks.push((
                format!("plugin `{}` daily", req.plugin),
                self.sum("created_at >= ? AND plugin = ?", &[ts(day), SqlArg::Text(req.plugin.clone())])?,
                cap.per_day_usd,
            ));
        }
        if let Some((scope, spent, cap)) = checks.into_iter().find(|(_, spent, cap)| spent + req.worst_usd > *cap) {
            return Err(self.reject(Reject::OverCap { scope, spent, worst: req.worst_usd, cap }, now));
        }

        let id = ulid::Ulid::generate().to_string();
        self.sql
            .exec(
                "INSERT INTO reservation (id, task, plugin, run_key, model, worst_usd, state, created_at)
                 VALUES (?, ?, ?, ?, ?, ?, 'reserved', ?)",
                &[
                    SqlArg::Text(id.clone()),
                    SqlArg::Text(req.task.clone()),
                    SqlArg::Text(req.plugin.clone()),
                    SqlArg::Text(req.run_key.clone()),
                    SqlArg::Text(req.model.clone()),
                    SqlArg::Real(req.worst_usd),
                    ts(now),
                ],
            )
            .map_err(LedgerError::Storage)?;
        Ok(Reservation { id, worst_usd: req.worst_usd })
    }

    fn reject(&self, r: Reject, now: DateTime<Utc>) -> LedgerError {
        if let Err(e) =
            self.sql.exec("INSERT INTO rejection (at, reason) VALUES (?, ?)", &[ts(now), SqlArg::Text(r.to_string())])
        {
            tracing::error!(error = %e, "failed to record rejection");
        }
        r.into()
    }

    /// Settle with the actual cost. `None` keeps the worst case charged.
    pub fn settle(&self, id: &str, actual_usd: Option<f64>, now: DateTime<Utc>) -> Result<(), LedgerError> {
        self.sql
            .exec(
                "UPDATE reservation SET state = 'settled', actual_usd = COALESCE(?, worst_usd), settled_at = ?
                 WHERE id = ? AND state = 'reserved'",
                &[actual_usd.map_or(SqlArg::Null, SqlArg::Real), ts(now), SqlArg::Text(id.into())],
            )
            .map(|_| ())
            .map_err(LedgerError::Storage)
    }

    pub fn spent_since(&self, since: DateTime<Utc>) -> Result<f64, LedgerError> {
        self.sum("created_at >= ?", &[ts(since)])
    }

    pub fn status(&self, now: DateTime<Utc>) -> Result<LedgerStatus, LedgerError> {
        let rej = self
            .sql
            .exec("SELECT COUNT(*) AS n FROM rejection WHERE at >= ?", &[ts(now - chrono::Duration::minutes(10))])
            .map_err(LedgerError::Storage)?;
        Ok(LedgerStatus {
            disabled: self.disabled()?,
            spent_today_usd: self.spent_since(day_start(now))?,
            spent_last_hour_usd: self.spent_since(now - chrono::Duration::hours(1))?,
            rejections_last_10m: rej.first().and_then(|r| r.get("n")).and_then(Value::as_u64).unwrap_or(0) as u32,
            key_checked_at: self.key_checked_at()?,
        })
    }

    /// Trip the breaker on abnormal spend rate or rejection storms. Returns the reason if tripped.
    pub fn breaker_check(&self, now: DateTime<Utc>) -> Result<Option<String>, LedgerError> {
        let s = self.status(now)?;
        let reason = if s.spent_last_hour_usd > self.cfg.breaker.max_usd_per_hour {
            Some(format!("breaker: ${:.4} spent in the last hour", s.spent_last_hour_usd))
        } else if s.rejections_last_10m > self.cfg.breaker.max_rejections_per_10m {
            Some(format!("breaker: {} rejections in 10 minutes", s.rejections_last_10m))
        } else {
            None
        };
        if let Some(r) = &reason {
            self.disable(r, now)?;
        }
        Ok(reason)
    }
}

#[cfg(test)]
pub(crate) mod sqlite {
    //! rusqlite-backed `LedgerSql` for native tests (same SQL as the Durable Object).
    use super::*;
    use rusqlite::types::{Value as SqlValue, ValueRef};

    pub struct MemSql(pub rusqlite::Connection);

    impl MemSql {
        pub fn new() -> Self {
            Self(rusqlite::Connection::open_in_memory().unwrap())
        }
    }

    impl LedgerSql for MemSql {
        fn exec(&self, sql: &str, args: &[SqlArg]) -> Result<Vec<Map<String, Value>>, String> {
            let params: Vec<SqlValue> = args
                .iter()
                .map(|a| match a {
                    SqlArg::Text(s) => SqlValue::Text(s.clone()),
                    SqlArg::Real(f) => SqlValue::Real(*f),
                    SqlArg::Null => SqlValue::Null,
                })
                .collect();
            let mut stmt = self.0.prepare(sql).map_err(|e| e.to_string())?;
            let names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
            let mut rows = stmt.query(rusqlite::params_from_iter(params)).map_err(|e| e.to_string())?;
            let mut out = Vec::new();
            while let Some(row) = rows.next().map_err(|e| e.to_string())? {
                let mut m = Map::new();
                for (i, n) in names.iter().enumerate() {
                    let v = match row.get_ref(i).map_err(|e| e.to_string())? {
                        ValueRef::Null => Value::Null,
                        ValueRef::Integer(x) => Value::from(x),
                        ValueRef::Real(x) => Value::from(x),
                        ValueRef::Text(t) => Value::from(String::from_utf8_lossy(t).into_owned()),
                        ValueRef::Blob(_) => Value::Null,
                    };
                    m.insert(n.clone(), v);
                }
                out.push(m);
            }
            Ok(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::sqlite::MemSql;
    use super::*;

    fn cfg() -> GatewayConfig {
        GatewayConfig::parse(
            r#"
            [global]
            per_day_usd = 0.10
            per_month_usd = 2.00
            per_request_max_usd = 0.05
            [[allow]]
            model = "a/cheap"
            max_prompt_usd_per_m = 1
            max_completion_usd_per_m = 4
            [plugins.digest]
            per_day_usd = 0.06
        "#,
        )
        .unwrap()
    }

    fn req(worst: f64) -> ReserveRequest {
        ReserveRequest {
            task: "nightly".into(),
            plugin: "digest".into(),
            run_key: "k".into(),
            model: "a/cheap".into(),
            worst_usd: worst,
            task_per_run_usd: 1.0,
            task_per_day_usd: 1.0,
        }
    }

    #[test]
    fn enforces_allowlist_and_request_cap() {
        let (sql, cfg) = (MemSql::new(), cfg());
        let l = Ledger::new(&sql, &cfg, None).unwrap();
        let now = Utc::now();
        let mut r = req(0.01);
        r.model = "b/expensive".into();
        assert!(matches!(l.reserve(&r, now), Err(LedgerError::Reject(Reject::ModelNotAllowed(_)))));
        assert!(matches!(l.reserve(&req(0.06), now), Err(LedgerError::Reject(Reject::OverRequest { .. }))));
    }

    #[test]
    fn unsettled_reservations_count_at_worst_case() {
        let (sql, cfg) = (MemSql::new(), cfg());
        let l = Ledger::new(&sql, &cfg, None).unwrap();
        let now = Utc::now();
        l.reserve(&req(0.04), now).unwrap();
        // plugin cap 0.06: 0.04 reserved + 0.04 > 0.06
        assert!(matches!(l.reserve(&req(0.04), now), Err(LedgerError::Reject(Reject::OverCap { .. }))));
        assert_eq!(l.status(now).unwrap().rejections_last_10m, 1);
    }

    #[test]
    fn settling_lower_frees_budget_and_unknown_keeps_worst() {
        let (sql, cfg) = (MemSql::new(), cfg());
        let l = Ledger::new(&sql, &cfg, None).unwrap();
        let now = Utc::now();
        let r = l.reserve(&req(0.04), now).unwrap();
        l.settle(&r.id, Some(0.001), now).unwrap();
        let r2 = l.reserve(&req(0.02), now).unwrap();
        l.settle(&r2.id, None, now).unwrap();
        assert!((l.status(now).unwrap().spent_today_usd - 0.021).abs() < 1e-9);
    }

    #[test]
    fn disable_is_sticky_until_a_later_deploy() {
        let (sql, cfg) = (MemSql::new(), cfg());
        let now = Utc::now();
        Ledger::new(&sql, &cfg, None).unwrap().disable("test", now).unwrap();
        let old_deploy = Some(now - chrono::Duration::hours(1));
        let l = Ledger::new(&sql, &cfg, old_deploy).unwrap();
        assert!(matches!(l.reserve(&req(0.01), now), Err(LedgerError::Reject(Reject::Disabled(_)))));
        let new_deploy = Some(now + chrono::Duration::seconds(1));
        let l = Ledger::new(&sql, &cfg, new_deploy).unwrap();
        assert!(l.reserve(&req(0.01), now).is_ok(), "a deploy with a later ENABLE_AFTER lifts it");
    }

    #[test]
    fn breaker_trips_on_spend_rate() {
        let mut cfg = cfg();
        cfg.breaker.max_usd_per_hour = 0.03;
        let sql = MemSql::new();
        let l = Ledger::new(&sql, &cfg, None).unwrap();
        let now = Utc::now();
        l.reserve(&req(0.04), now).unwrap();
        assert!(l.breaker_check(now).unwrap().is_some());
        assert!(l.disabled().unwrap().is_some());
    }
}
