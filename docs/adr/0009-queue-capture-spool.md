# 0009: A Cloudflare Queue as the capture spool

- **Status:** accepted; superseded by 0014 (self-hosted VPS)
- **Date:** 2026-11-23

## Context
Invariant 1 says a capture is never lost after auth. A database that's paused (Earth plan limits), unreachable or slow must not drop captures.

## Decision
- Ingest routes resolve the capture purely, without touching the database. That's where the item id is decided.
- They then put `CaptureMsg { capture, item }` on the **CAPTURES** queue and reply `202`.
- A consumer persists the message idempotently: the same ids on redelivery produce Conflict, which counts as done.
- Retries are capped at 10, and a **dead-letter queue** keeps anything that still fails.

## Consequences
- The reply can't include the stored item, only the planned one. The UI learns of the write through the Hub.
- Ordering isn't guaranteed. That's fine, because captures are independent.
