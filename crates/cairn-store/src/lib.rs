//! Persistence boundary. Everything outside this crate talks to `dyn Store`.
//!
//! - [`MemStore`]: in-memory implementation, used by unit tests across the workspace.
//! - [`SurrealHttpStore`]: SurrealDB 3.x/4.x over HTTP `/rpc` (ADR 0011), generic over a
//!   [`cairn_http::HttpTransport`] so it runs in Workers and in native tests.
//!
//! Invariant: every mutation goes through [`Store::apply`], which persists the change *and*
//! appends to the ops log in one transaction. Captures are the one exception: they are
//! written with [`Store::put_capture`] so ingest can never be blocked by op validation.

mod mem;
mod surreal_cbor;
mod surreal_http;

pub use mem::MemStore;
pub use surreal_http::{SchemaFile, SurrealConfig, SurrealHttpStore};

use async_trait::async_trait;
use cairn_domain::{Capture, Edge, Id, Item, Op, Query, View};
use chrono::{DateTime, Utc};
use thiserror::Error;

pub type Result<T> = std::result::Result<T, StoreError>;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("forbidden op: {0}")]
    Forbidden(String),
    #[error("invalid query: {0}")]
    InvalidQuery(String),
    #[error("not implemented yet: {0}")]
    Unimplemented(&'static str),
    #[error("database error: {0}")]
    Db(String),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
}

/// Result of applying a batch of ops: the batch id is the undo unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Applied {
    pub batch: Id,
    pub count: usize,
}

#[async_trait]
pub trait Store: Send + Sync + 'static {
    /// Apply ops atomically and append them to the ops log. Rejects `ItemPurge` unless the
    /// actor is `Actor::Gc`.
    async fn apply(&self, ops: Vec<Op>) -> Result<Applied>;

    async fn get_item(&self, id: Id) -> Result<Option<Item>>;

    /// `now` is explicit so relative times resolve deterministically (dry-runs, tests).
    async fn query_items(&self, query: &Query, now: DateTime<Utc>) -> Result<Vec<Item>>;

    /// All edges touching `id`, in either direction.
    async fn edges_of(&self, id: Id) -> Result<Vec<Edge>>;

    async fn get_view(&self, id: Id) -> Result<Option<View>>;

    /// Store a raw capture. Must not validate the body (invariant 1).
    async fn put_capture(&self, capture: Capture) -> Result<()>;

    async fn resolve_capture(&self, id: Id, item: Option<Id>, error: Option<String>) -> Result<()>;

    /// GC: delete resolved raw captures received before `before`. Returns how many were deleted.
    /// Unresolved captures are never deleted automatically.
    async fn purge_resolved_captures(&self, before: DateTime<Utc>) -> Result<usize>;
}

/// Shared check used by every implementation before persisting.
pub(crate) fn check_ops(ops: &[Op]) -> Result<()> {
    for op in ops {
        if op.requires_gc() && op.actor != cairn_domain::Actor::Gc {
            return Err(StoreError::Forbidden(format!(
                "{} may not purge items; only gc can",
                op.actor.as_log_string()
            )));
        }
    }
    Ok(())
}
