//! Cairn on the VPS (ADR 0014). One persistent process:
//!
//! - HTTP: the app router (`cairn_app::api`), `/api/v1/admin/migrate`, `/hub` WebSockets,
//!   and the web app from `CAIRN_WEB_DIR` (SPA fallback to `index.html`).
//! - A durable SQLite spool for captures and task firings, drained in the background.
//! - A scheduler timer for reminders and cron tasks.
//!
//! Commands: `cairn` (serve), `cairn migrate`, `cairn spool status`, `cairn spool retry-dead`.

mod background;
mod gateway;
mod hub;
mod spool;

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::{get, post};
use cairn_app::auth::{Scope, TokenSet};
use cairn_app::discord::DiscordKeys;
use cairn_app::error::ApiError;
use cairn_app::{AppState, CoreConfig};
use cairn_http::native::ReqwestTransport;
use cairn_plugins::Registry;
use cairn_store::{AuthLevel, SchemaFile, SurrealConfig, SurrealHttpStore};
use include_dir::{Dir, include_dir};
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;
use tower_http::services::{ServeDir, ServeFile};

static SCHEMA: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../../schema");
const DEFAULT_CONFIG: &str = include_str!("../../../config/cairn.toml");

type Surreal = SurrealHttpStore<ReqwestTransport>;

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn need(name: &str) -> Result<String, String> {
    env(name).ok_or_else(|| format!("{name} is not set"))
}

fn schema_files() -> Vec<(String, String)> {
    let mut files: Vec<(String, String)> = SCHEMA
        .files()
        .filter(|f| f.path().extension().is_some_and(|e| e == "surql"))
        .filter_map(|f| Some((f.path().file_name()?.to_string_lossy().into_owned(), f.contents_utf8()?.to_owned())))
        .collect();
    files.sort();
    files
}

async fn migrate(store: &Surreal) -> Result<Vec<String>, cairn_store::StoreError> {
    let files = schema_files();
    let refs: Vec<SchemaFile<'_>> = files.iter().map(|(n, s)| SchemaFile { name: n, sql: s }).collect();
    store.migrate(&refs).await
}

/// `config/cairn.toml` is embedded; `CAIRN_CONFIG` points at a file to use instead, so the
/// VPS can change config without a rebuild.
fn config() -> Result<CoreConfig, String> {
    match env("CAIRN_CONFIG") {
        Some(p) => {
            let raw = std::fs::read_to_string(&p).map_err(|e| format!("{p}: {e}"))?;
            CoreConfig::parse(&raw).map_err(|e| format!("{p}: {e}"))
        }
        None => CoreConfig::parse(DEFAULT_CONFIG).map_err(|e| format!("config/cairn.toml: {e}")),
    }
}

fn surreal(cfg: &CoreConfig) -> Result<Arc<Surreal>, String> {
    let s = cfg.surreal.as_ref().ok_or("[surreal] missing in config")?;
    let http = Arc::new(ReqwestTransport::new(Duration::from_secs(30)).map_err(|e| e.to_string())?);
    Ok(Arc::new(SurrealHttpStore::new(
        http,
        SurrealConfig {
            url: env("SURREAL_URL").unwrap_or_else(|| s.url.clone()),
            namespace: env("SURREAL_NS").unwrap_or_else(|| s.namespace.clone()),
            database: env("SURREAL_DB").unwrap_or_else(|| s.database.clone()),
            username: need("SURREAL_USER")?,
            password: need("SURREAL_PASS")?,
            auth: match env("SURREAL_AUTH_LEVEL") {
                Some(l) => AuthLevel::parse(&l).ok_or("SURREAL_AUTH_LEVEL: root | namespace | database")?,
                None => AuthLevel::Database,
            },
        },
    )))
}

struct Built {
    app: AppState,
    store: Arc<Surreal>,
    spool: Arc<spool::SqliteSpool>,
    hub: hub::BroadcastHub,
    wake: Arc<Notify>,
}

fn build() -> Result<Built, String> {
    // Invariant 5: this process must never hold a spend credential.
    for name in ["OPENROUTER_API_KEY", "CF_AIG_TOKEN"] {
        if env(name).is_some() {
            return Err(format!("{name} is set for cairn; it belongs only to cairn-gateway"));
        }
    }
    let cfg = Arc::new(config()?);
    let store = surreal(&cfg)?;
    let plugins = Registry::load_from(&cairn_plugins_embedded::Embedded).map_err(|e| format!("plugins: {e}"))?;
    let spool_path = PathBuf::from(env("SPOOL_PATH").unwrap_or_else(|| "/data/spool.sqlite".into()));
    let spool =
        Arc::new(spool::SqliteSpool::open(&spool_path).map_err(|e| format!("spool {}: {e}", spool_path.display()))?);
    let hub = hub::BroadcastHub::new();
    let wake = Arc::new(Notify::new());
    let http: Arc<dyn cairn_http::HttpTransport> =
        Arc::new(ReqwestTransport::new(Duration::from_secs(30)).map_err(|e| e.to_string())?);
    let discord = if cfg.discord.enabled {
        Some(DiscordKeys::new(&need("DISCORD_PUBLIC_KEY")?, need("DISCORD_BOT_TOKEN")?, &cfg.discord.dm_user_id)?)
    } else {
        None
    };
    // Model calls can be slow; this bounds a whole completion through the gateway.
    let gateway = match env("GATEWAY_URL") {
        Some(base) => {
            let http = Arc::new(ReqwestTransport::new(Duration::from_secs(180)).map_err(|e| e.to_string())?);
            Some(Arc::new(gateway::GatewayHttp { http, base }) as Arc<dyn cairn_app::ports::Gateway>)
        }
        None => None,
    };
    let app = AppState {
        cfg,
        store: store.clone(),
        plugins: Arc::new(plugins),
        tokens: Arc::new(TokenSet::from_env_str(&env("CAIRN_TOKENS").unwrap_or_default())),
        spool: Arc::new(spool::SpoolPort(spool.clone())),
        hub: Arc::new(hub.clone()),
        scheduler: Arc::new(background::TimerScheduler(wake.clone())),
        gateway,
        discord,
        http,
    };
    Ok(Built { app, store, spool, hub, wake })
}

fn router(b: &Built, web_dir: PathBuf) -> Router {
    let store = b.store.clone();
    let admin = Router::new()
        .route(
            "/api/v1/admin/migrate",
            post(move |State(st): State<AppState>, headers: HeaderMap| {
                let store = store.clone();
                async move {
                    st.tokens.check(&headers, Scope::Admin)?;
                    let ran = migrate(&store).await?;
                    Ok::<_, ApiError>(Json(json!({ "applied": ran })))
                }
            }),
        )
        .with_state(b.app.clone());
    let hub = Router::new().route("/hub", get(hub::upgrade)).with_state((b.app.clone(), b.hub.clone()));
    let web = ServeDir::new(&web_dir).fallback(ServeFile::new(web_dir.join("index.html")));
    cairn_app::api::router().with_state(b.app.clone()).merge(admin).merge(hub).fallback_service(web)
}

async fn shutdown() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = term => {} }
}

#[tokio::main]
async fn main() -> Result<(), String> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let b = build()?;
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        [] | ["serve"] => {}
        ["migrate"] => {
            let ran = migrate(&b.store).await.map_err(|e| e.to_string())?;
            println!("{}", json!({ "applied": ran }));
            return Ok(());
        }
        ["spool", "status"] => {
            println!("{}", json!(b.spool.counts().map_err(|e| e.to_string())?));
            return Ok(());
        }
        ["spool", "retry-dead"] => {
            let n = b.spool.revive_dead(chrono::Utc::now()).map_err(|e| e.to_string())?;
            println!("{}", json!({ "revived": n }));
            return Ok(());
        }
        other => {
            return Err(format!("unknown command {other:?}; try: serve | migrate | spool status | spool retry-dead"));
        }
    }

    let web_dir = PathBuf::from(env("CAIRN_WEB_DIR").unwrap_or_else(|| "/app/web".into()));
    let bind = env("CAIRN_BIND").unwrap_or_else(|| "0.0.0.0:8787".into());
    tokio::spawn(background::consume(b.app.clone(), b.spool.clone()));
    tokio::spawn(background::schedule(b.app.clone(), b.spool.clone(), b.wake.clone()));
    let listener = tokio::net::TcpListener::bind(&bind).await.map_err(|e| format!("bind {bind}: {e}"))?;
    let counts = b.spool.counts().unwrap_or_default();
    tracing::info!(%bind, web = %web_dir.display(), pending = counts.pending, dead = counts.dead, gateway = b.app.gateway.is_some(), "cairn listening");
    axum::serve(listener, router(&b, web_dir)).with_graceful_shutdown(shutdown()).await.map_err(|e| e.to_string())?;
    Ok(())
}
