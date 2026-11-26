use crate::auth::TokenSet;
use crate::config::CoreConfig;
use crate::discord::DiscordKeys;
use crate::ports::{Gateway, Hub, SchedulerPort, Spool};
use cairn_http::HttpTransport;
use cairn_plugins::Registry;
use cairn_store::Store;
use std::sync::Arc;

/// Everything a request handler can reach. Built per request (or cached per isolate) by the
/// API Worker from its bindings; built from in-memory parts in tests.
#[derive(Clone)]
pub struct AppState {
    pub cfg: Arc<CoreConfig>,
    pub store: Arc<dyn Store>,
    pub plugins: Arc<Registry>,
    pub tokens: Arc<TokenSet>,
    pub spool: Arc<dyn Spool>,
    pub hub: Arc<dyn Hub>,
    pub scheduler: Arc<dyn SchedulerPort>,
    pub gateway: Option<Arc<dyn Gateway>>,
    pub discord: Option<DiscordKeys>,
    /// Outbound HTTP (Discord REST). Workers fetch in production.
    pub http: Arc<dyn HttpTransport>,
}
