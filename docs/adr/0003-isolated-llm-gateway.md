# 0003: An isolated LLM gateway for spend control

- **Status:** accepted (revised for Workers)
- **Date:** 2026-11-23

## Context
Spend limits must not be bypassable by code, whether through plugins, tasks, bugs or a compromised API Worker.

## Decision
- `cairn-gateway` is its own Worker.
  - It is the only Worker with a spend credential: `CF_AIG_TOKEN` (the OpenRouter key itself lives in AI Gateway BYOK, ADR 0012).
  - It has **no public URL** and is reached only through a **Service Binding** from the API Worker.
- **Caps are compiled into the build** from `config/gateway.toml`.
- The ledger is a **SQLite-backed Durable Object**. It serializes reservations, and only the gateway is bound to it.
- There's a disable endpoint and no enable endpoint. A disable is lifted only by deploying a later `ENABLE_AFTER`.
- The gateway refuses (and disables itself on) management keys and keys without a credit limit.

## Consequences
- An extra hop through the binding on every AI call, which is negligible next to model latency.
- Raising any limit needs your Cloudflare credentials. This is intentional.
- See docs/spend-control.md.
