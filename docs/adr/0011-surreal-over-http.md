# 0011: Reach SurrealDB over HTTP `/rpc`, not the SDK's WebSocket

- **Status:** accepted
- **Date:** 2026-11-23

## Context
Workers are short-lived and stateless. SurrealDB's own guidance for serverless code is HTTP, and the Rust SDK's WebSocket engine is a poor fit for `wasm32-unknown-unknown` inside Workers.

## Decision
- `SurrealHttpStore<T: HttpTransport>` sends one stateless `POST /rpc` (JSON-RPC `query`) per operation, with namespace and database headers and Basic auth for a database-level user.
- A batch of ops is one request containing `BEGIN … COMMIT`.
- Variables are always bound. Field names that end up in the query text are checked against a strict pattern first.

## Consequences
- No connection state, which works well with Workers.
- The RPC envelope and header names must be **verified against a live instance in M1**.
- The same store runs natively in tests through `FakeTransport`.
