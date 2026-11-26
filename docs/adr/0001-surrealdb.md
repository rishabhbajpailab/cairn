# 0001: SurrealDB on Surreal Cloud as the primary store

- **Status:** accepted (revised by 0011 for transport)
- **Date:** 2026-11-23

## Context
The data is document-shaped (plugin payloads), graph-shaped (typed edges) and needs a durable, queryable history. The system is cloud-native: nothing runs on your own hardware.

## Decision
- Use SurrealDB 3.x/4.x hosted on **Surreal Cloud**.
- Edges go in a `RELATION` table (`link`). Payloads are schemaless (`FLEXIBLE`) and core fields are schemafull. An append-only guard on `op` is a table event.
- All access goes through the `Store` trait.

## Consequences
- You get graph traversal, document storage and table events in one place. Live updates come from the Hub Durable Object, not from database live queries (ADR 0008).
- With no polling and no open connections, the database is touched only when there's work to do, so Earth's active-hours allowance is realistic. A dedicated Cloud node (always on) is the fallback if Earth lacks a feature we rely on, table events in particular. **Verify in M1.**
- `MemStore` backs unit tests, so most tests need no database at all.
