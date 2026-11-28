//! Cairn application logic, independent of where it runs.
//!
//! The API Worker (`workers/api`) builds an [`AppState`] from Cloudflare bindings and calls:
//! - [`api::router`] for HTTP requests
//! - [`ingest::process_capture`] from the CAPTURES queue consumer
//! - [`reminders::fire_due`] / [`reminders::next_wake`] / [`schedule::due_cron`] from the
//!   Scheduler Durable Object's alarm
//!
//! Everything here compiles natively, so the whole flow is unit-tested with `MemStore` and
//! the in-memory ports in [`ports`].

pub mod ai_apply;
pub mod api;
pub mod auth;
pub mod config;
pub mod discord;
pub mod error;
pub mod ingest;
pub mod items;
pub mod onboarding;
pub mod ports;
pub mod reminders;
pub mod schedule;
pub mod state;

pub use config::CoreConfig;
pub use state::AppState;

#[cfg(test)]
pub(crate) mod testing {
    use crate::auth::{TokenSet, sha256_hex};
    use crate::ports::{CountingScheduler, RecordingHub, VecSpool};
    use crate::state::AppState;
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use cairn_http::FakeTransport;
    use cairn_store::MemStore;
    use http_body_util::BodyExt;
    use serde_json::Value;
    use std::sync::Arc;
    use tower::ServiceExt;

    pub struct Harness {
        pub state: AppState,
        pub store: Arc<MemStore>,
        pub spool: Arc<VecSpool>,
        pub hub: Arc<RecordingHub>,
        pub scheduler: Arc<CountingScheduler>,
        pub http: Arc<FakeTransport>,
    }

    pub fn harness_with(cfg_toml: &str, http: FakeTransport) -> Harness {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
        let store = Arc::new(MemStore::new());
        let spool = Arc::new(VecSpool::default());
        let hub = Arc::new(RecordingHub::default());
        let scheduler = Arc::new(CountingScheduler::default());
        let http = Arc::new(http);
        let state = AppState {
            cfg: Arc::new(crate::CoreConfig::parse(cfg_toml).unwrap()),
            store: store.clone(),
            plugins: Arc::new(cairn_plugins::Registry::load(&root).unwrap()),
            tokens: Arc::new(TokenSet::from_env_str(&format!(
                "phone:capture:{};ui:write:{}",
                sha256_hex("cap"),
                sha256_hex("ui")
            ))),
            spool: spool.clone(),
            hub: hub.clone(),
            scheduler: scheduler.clone(),
            gateway: None,
            discord: None,
            http: http.clone(),
        };
        Harness { state, store, spool, hub, scheduler, http }
    }

    pub fn harness() -> Harness {
        harness_with("timezone = \"America/Los_Angeles\"", FakeTransport::default())
    }

    pub async fn call(app: Router, req: Request<Body>) -> (StatusCode, Value) {
        let resp = app.oneshot(req).await.unwrap();
        let status = resp.status();
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
    }
}
