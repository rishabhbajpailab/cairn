//! Cairn API Worker (ADR 0008).
//!
//! - `fetch`: `/api/*`, `/ingest/*`, `/integrations/*`, `/hub`, `/healthz`. Everything else
//!   is served by Workers static assets (web app + plugin `views.js`) before reaching here
//!   (`assets.run_worker_first` in wrangler.toml).
//! - `queue`: CAPTURES (persist captures) and TASKS (task firings, M5).
//! - `scheduled`: hourly backstop that re-arms the Scheduler Durable Object.
//! - Durable Objects: [`Scheduler`], [`Hub`].

mod embedded;
mod hub_do;
mod ports;
mod scheduler_do;
mod state;
mod transport;

pub use hub_do::Hub;
pub use scheduler_do::Scheduler;

use axum::Json;
use axum::extract::State as AxState;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use cairn_app::AppState;
use cairn_app::auth::Scope;
use cairn_app::error::ApiError;
use cairn_app::ports::CaptureMsg;
use cairn_app::schedule::TaskFire;
use serde_json::{Value, json};
use std::sync::Arc;
use tower_service::Service;
use worker::send::SendFuture;
use worker::{Context, Env, HttpRequest, MessageBatch, MessageExt, ScheduleContext, ScheduledEvent, event};

/// Admin: apply embedded `schema/*.surql` (run once after deploy: `make migrate`).
async fn migrate(
    AxState(st): AxState<AppState>,
    headers: HeaderMap,
    store: Arc<cairn_store::SurrealHttpStore<transport::WorkerFetch>>,
) -> Result<Json<Value>, ApiError> {
    st.tokens.check(&headers, Scope::Admin)?;
    let files = embedded::schema_files();
    let refs: Vec<cairn_store::SchemaFile<'_>> =
        files.iter().map(|(n, s)| cairn_store::SchemaFile { name: n, sql: s }).collect();
    let ran = store.migrate(&refs).await?;
    Ok(Json(json!({ "applied": ran })))
}

#[event(fetch)]
async fn fetch(req: HttpRequest, env: Env, _ctx: Context) -> worker::Result<axum::http::Response<axum::body::Body>> {
    // Live updates: authenticate, then hand the WebSocket upgrade to the Hub DO.
    if req.uri().path() == "/hub" {
        return hub_upgrade(req, &env).await;
    }
    let app = state::build(&env, false)?;
    let cfg = app.cfg.clone();
    let surreal = state::surreal_store(&env, &cfg)?;
    let mut router = cairn_app::api::router()
        .route(
            "/api/v1/admin/migrate",
            post(move |st: AxState<AppState>, headers: HeaderMap| {
                let store = surreal.clone();
                async move { SendFuture::new(migrate(st, headers, store)).await }
            }),
        )
        .with_state(app);
    Ok(router.call(req).await?)
}

async fn hub_upgrade(req: HttpRequest, env: &Env) -> worker::Result<axum::http::Response<axum::body::Body>> {
    let app = state::build(env, false)?;
    // Browsers can't set headers on WebSocket upgrades: accept `?token=` and lift it to a header.
    let token =
        req.uri().query().and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("token="))).unwrap_or("").to_string();
    let mut h = HeaderMap::new();
    if let Ok(v) = format!("Bearer {token}").parse() {
        h.insert(axum::http::header::AUTHORIZATION, v);
    }
    if app.tokens.check(&h, Scope::Read).is_err() {
        return axum::http::Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .body(axum::body::Body::empty())
            .map_err(|e| worker::Error::RustError(e.to_string()));
    }
    let stub = env.durable_object("HUB")?.id_from_name("hub")?.get_stub()?;
    let wreq: worker::Request = req.try_into()?;
    let resp = stub.fetch_with_request(wreq).await?;
    let resp: axum::http::Response<worker::Body> = resp.try_into()?;
    Ok(resp.map(axum::body::Body::new))
}

#[event(queue)]
async fn queue(batch: MessageBatch<Value>, env: Env, _ctx: Context) -> worker::Result<()> {
    let app = state::build(&env, false)?;
    let queue_name = batch.queue();
    for msg in batch.messages()? {
        let outcome = if queue_name.ends_with("captures") {
            match serde_json::from_value::<CaptureMsg>(msg.body().clone()) {
                Ok(m) => cairn_app::ingest::process_capture(&app, &m).await.map_err(|e| e.to_string()),
                Err(e) => {
                    // Malformed forever: ack so it doesn't loop; the DLQ in wrangler.toml keeps a copy.
                    tracing::error!(error = %e, "undecodable capture message");
                    Ok(())
                }
            }
        } else if queue_name.ends_with("tasks") {
            match serde_json::from_value::<TaskFire>(msg.body().clone()) {
                Ok(f) => {
                    // TODO(M5): resolve input, compute run key, fan out chunks, call gateway, apply.
                    tracing::info!(task = %f.task, at = %f.at, "task fired (runner lands in M5)");
                    Ok(())
                }
                Err(e) => Err(e.to_string()),
            }
        } else {
            Err(format!("unknown queue {queue_name}"))
        };
        match outcome {
            Ok(()) => msg.ack(),
            Err(e) => {
                tracing::warn!(error = %e, "message failed; will retry");
                msg.retry();
            }
        }
    }
    Ok(())
}

/// Hourly backstop: re-arm the Scheduler in case an alarm was lost or a rearm call failed.
/// TODO(M6): nightly `builtin/export` to R2 from here.
#[event(scheduled)]
async fn scheduled(_ev: ScheduledEvent, env: Env, _ctx: ScheduleContext) {
    let r = async {
        let stub = env.durable_object("SCHEDULER")?.id_from_name("scheduler")?.get_stub()?;
        stub.fetch_with_str("https://scheduler/rearm").await
    }
    .await;
    if let Err(e) = r {
        tracing::error!(error = %e, "cron backstop could not reach Scheduler");
    }
}
