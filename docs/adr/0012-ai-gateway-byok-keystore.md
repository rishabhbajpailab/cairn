# 0012: Cloudflare AI Gateway BYOK as the OpenRouter keystore

- **Status:** accepted (refines 0003)
- **Date:** 2026-11-25

## Context
The OpenRouter key was a Worker secret on the gateway. Cloudflare AI Gateway can store provider keys in Secrets Store and inject them at request time (BYOK). It also adds logs, analytics and spend-limit rules, and it makes the key easy to share and rotate across the Cloudflare account without building any key-sharing infrastructure.

## Decision
- The OpenRouter key lives in AI Gateway's Provider Keys under a **dedicated `cairn` alias**, backed by a dedicated OpenRouter key with its own credit limit.
- The gateway Worker binds only `CF_AIG_TOKEN`. It calls `AIG_BASE_URL` (the AI Gateway OpenRouter endpoint) with `cf-aig-authorization` and `cf-aig-byok-alias`, plus `cf-aig-metadata` for per-task analytics, and **never** a provider `Authorization` header.
- The gateway Worker stays the exact policy layer: compiled-in caps, worst-case reservations in the Ledger DO, allowlist, fixed routing, and a disable that's lifted only by a deploy. An AI Gateway spend-limit rule sits slightly above the ledger's daily cap as an independent outer net, and the OpenRouter key limit is the hard stop.
- `OPENROUTER_API_KEY` remains only for local dev. Binding both credentials is refused.
- The hourly key check goes through AI Gateway passthrough. `key_check = required | best-effort` decides what happens if `/key` isn't proxied.

## Consequences
- No Worker, log or `wrangler` config ever contains the raw OpenRouter key. Rotating it is a dashboard edit with no deploy.
- `CF_AIG_TOKEN` is now the spend credential to protect. It's capped by layers 1 and 2 even if it leaks.
- AI Gateway spend limits are eventually consistent and count cost after the fact, which is why they're the outer net and the ledger stays primary.
- Other apps can share the AI Gateway account, but should use their own key alias so Cairn's reconcile stays meaningful.
- **To verify in M1:** BYOK injection for OpenRouter, whether the base URL needs `/v1`, `/key` passthrough, and whether metadata reaches spend-limit rules.
