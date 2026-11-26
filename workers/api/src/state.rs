//! Build `cairn_app::AppState` from Worker bindings. Parsed config and the plugin registry
//! are cached per isolate.

use crate::embedded::{CORE_CONFIG, Embedded};
use crate::ports::{GatewayBinding, HubPort, NoopScheduler, QueueSpool, SchedulerStub};
use crate::transport::WorkerFetch;
use cairn_app::auth::TokenSet;
use cairn_app::discord::DiscordKeys;
use cairn_app::ports::SchedulerPort;
use cairn_app::{AppState, CoreConfig};
use cairn_plugins::Registry;
use cairn_store::{SurrealConfig, SurrealHttpStore};
use std::cell::OnceCell;
use std::sync::Arc;
use worker::send::SendWrapper;
use worker::{Env, Error, Result};

thread_local! {
    static CONFIG: OnceCell<Arc<CoreConfig>> = const { OnceCell::new() };
    static PLUGINS: OnceCell<Arc<Registry>> = const { OnceCell::new() };
}

fn config() -> Result<Arc<CoreConfig>> {
    CONFIG.with(|c| {
        if let Some(v) = c.get() {
            return Ok(v.clone());
        }
        let v =
            Arc::new(CoreConfig::parse(CORE_CONFIG).map_err(|e| Error::RustError(format!("config/cairn.toml: {e}")))?);
        let _ = c.set(v.clone());
        Ok(v)
    })
}

fn plugins() -> Result<Arc<Registry>> {
    PLUGINS.with(|c| {
        if let Some(v) = c.get() {
            return Ok(v.clone());
        }
        let v = Arc::new(Registry::load_from(&Embedded).map_err(|e| Error::RustError(format!("plugins: {e}")))?);
        let slow = v.runtime_compiled();
        if !slow.is_empty() {
            // Costs CPU on every cold isolate; see docs/deploy.md § CPU limits.
            tracing::warn!(?slow, "plugin schemas compiled at runtime instead of build time");
        }
        let _ = c.set(v.clone());
        Ok(v)
    })
}

fn secret(env: &Env, name: &str) -> Option<String> {
    env.secret(name).ok().map(|s| s.to_string())
}

pub fn surreal_store(env: &Env, cfg: &CoreConfig) -> Result<Arc<SurrealHttpStore<WorkerFetch>>> {
    let s = cfg.surreal.as_ref().ok_or_else(|| Error::RustError("[surreal] missing in config/cairn.toml".into()))?;
    Ok(Arc::new(SurrealHttpStore::new(
        Arc::new(WorkerFetch),
        SurrealConfig {
            // `SURREAL_URL` (var or .dev.vars) overrides the committed URL, e.g. a local
            // `surreal start` during `wrangler dev`.
            url: env
                .var("SURREAL_URL")
                .ok()
                .map(|v| v.to_string())
                .filter(|u| !u.is_empty())
                .unwrap_or_else(|| s.url.clone()),
            namespace: s.namespace.clone(),
            database: s.database.clone(),
            username: secret(env, "SURREAL_USER").ok_or_else(|| Error::RustError("SURREAL_USER secret".into()))?,
            password: secret(env, "SURREAL_PASS").ok_or_else(|| Error::RustError("SURREAL_PASS secret".into()))?,
        },
    )))
}

/// `inside_scheduler`: the Scheduler DO builds state with a no-op scheduler port so its own
/// writes don't call back into itself.
pub fn build(env: &Env, inside_scheduler: bool) -> Result<AppState> {
    // Invariant 5: this Worker must never hold a spend credential.
    for name in ["OPENROUTER_API_KEY", "CF_AIG_TOKEN"] {
        if env.secret(name).is_ok() || env.var(name).is_ok() {
            return Err(Error::RustError(format!(
                "{name} is bound to the API Worker; it belongs only to cairn-gateway"
            )));
        }
    }
    let cfg = config()?;
    let scheduler: Arc<dyn SchedulerPort> = if inside_scheduler {
        Arc::new(NoopScheduler)
    } else {
        Arc::new(SchedulerStub(SendWrapper::new(env.durable_object("SCHEDULER")?)))
    };
    let discord = if cfg.discord.enabled {
        let pk = secret(env, "DISCORD_PUBLIC_KEY").ok_or_else(|| Error::RustError("DISCORD_PUBLIC_KEY".into()))?;
        let bot = secret(env, "DISCORD_BOT_TOKEN").ok_or_else(|| Error::RustError("DISCORD_BOT_TOKEN".into()))?;
        Some(DiscordKeys::new(&pk, bot, &cfg.discord.dm_user_id).map_err(Error::RustError)?)
    } else {
        None
    };
    Ok(AppState {
        store: surreal_store(env, &cfg)?,
        plugins: plugins()?,
        tokens: Arc::new(TokenSet::from_env_str(&secret(env, "CAIRN_TOKENS").unwrap_or_default())),
        spool: Arc::new(QueueSpool(SendWrapper::new(env.queue("CAPTURES")?))),
        hub: Arc::new(HubPort(SendWrapper::new(env.durable_object("HUB")?))),
        scheduler,
        gateway: env.service("GATEWAY").ok().map(|f| Arc::new(GatewayBinding(SendWrapper::new(f))) as _),
        discord,
        http: Arc::new(WorkerFetch),
        cfg,
    })
}
