# 0011: Reach SurrealDB over HTTP `/rpc`, not the SDK's WebSocket

- **Status:** accepted
- **Date:** 2026-11-23

## Context
Workers are short-lived and stateless. SurrealDB's own guidance for serverless code is HTTP, and the Rust SDK's WebSocket engine is a poor fit for `wasm32-unknown-unknown` inside Workers.

## Decision
- `SurrealHttpStore<T: HttpTransport>` sends one stateless `POST /rpc` (RPC `query`) per operation, with namespace and database headers and Basic auth for a database-level user.
- **The body is CBOR, not JSON** (amended 2026-11-26, after the first live run). SurrealDB parses JSON request values with its SurrealQL value parser (`legacy_strands`), so any string that looks like a record id or a datetime is retyped, nested values included. That broke `TYPE string` fields such as `op.target`, and would have silently retyped user payloads and raw capture bodies (invariant 1). CBOR text is always a string. Responses are decoded back into JSON-shaped values (records become `"table:key"`, datetimes become RFC 3339 strings, NONE becomes null; see `surreal_cbor.rs`).
- A batch of ops is one request containing `BEGIN … COMMIT`.
- Variables are always bound. Field names that end up in the query text are checked against a strict pattern first.

## Consequences
- No connection state, which works well with Workers.
- Verified against SurrealDB 4.0.1 by `crates/cairn-store/tests/live_surreal.rs` (`make test-live`). The envelope (`{id, method, params}` → `{id, result: [{status, time, result}]}`), the lowercase `surreal-ns` / `surreal-db` headers, `type::record`, `record::id`, `INSERT RELATION`, `FULLTEXT … BM25`, the unique edge index and the `op_append_only` event all behave as written.
- Other things the live run showed: namespace and database must exist beforehand (`make db-init` locally), and `option<…>` fields accept NONE (an absent key) but reject NULL, so the store omits keys rather than sending `null`.
- Because values are no longer coerced, plugin `idx` datetimes are stored and compared as RFC 3339 strings. That orders correctly only if they share a format, so normalise them to UTC `Z` (TODO before M2's table filters).
- The same store runs natively in tests through `FakeTransport`.
