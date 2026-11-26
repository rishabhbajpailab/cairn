//! Spend-control core for `workers/gateway` (docs/spend-control.md, ADR 0003).
//!
//! - [`config`]: caps compiled into the Worker from `config/gateway.toml`
//! - [`ledger`]: reserve/settle/disable over a SQL trait (Durable Object SQLite in prod)
//! - [`select`]: order a task's models; never adds one
//! - [`openrouter`]: OpenRouter client over [`cairn_http::HttpTransport`]
//! - [`service`]: the request flow tying them together

pub mod config;
pub mod ledger;
pub mod openrouter;
pub mod select;
pub mod service;
