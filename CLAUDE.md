# CLAUDE.md

Cairn is a single-user, **cloud-native** "second brain" on **Cloudflare Workers + Durable Objects + Queues**, with **SurrealDB on Surreal Cloud** and **OpenRouter** behind an isolated gateway Worker. The gateway reaches OpenRouter through **Cloudflare AI Gateway**, using a key stored there (BYOK). It captures items from REST, webhooks and Discord into a typed graph, shows them on a visual canvas, and runs deterministic, budget-capped AI tasks over them. Start with `docs/ARCHITECTURE.md`. The current milestone is in `docs/ROADMAP.md` (M1: first live run against Surreal, plus wasm builds).

## Layout
```
crates/cairn-domain        pure types (Item, Edge, View, Op, Capture, Query): no I/O
crates/cairn-http          HttpTransport trait + FakeTransport (tests)
crates/cairn-store         Store trait, MemStore (tests), SurrealHttpStore (/rpc over HTTP)
crates/cairn-plugins       plugin registry (fs or embedded), JSON Schema, idx, capture matchers
crates/cairn-plugins-embedded  plugins/ embedded + JSON Schemas compiled to Rust at build time
crates/cairn-scheduler     TaskDef, DAG lint, cron, next_fire_after, run keys
crates/cairn-gc            lifecycle planner (pure)
crates/cairn-app           platform-agnostic app: router, ingest, consumer, reminders, Discord, ai_apply
crates/cairn-gateway-core  caps, ledger (SQL trait), model selection, OpenRouter client, service flow
workers/api                Worker glue: bindings → ports; fetch/queue/scheduled; Scheduler + Hub DOs
workers/gateway            Worker glue: Ledger DO, Service-Binding-only entry, fetch transport
plugins/types/*            type plugins (plugin.json, schema.json, views.js)
plugins/ai/*               AI capabilities (plugin.json, prompt.md, output.schema.json)
schema/*.surql             SurrealDB schema, applied in order via POST /api/v1/admin/migrate
config/cairn.toml          API Worker config (committed, embedded at build time)
config/gateway.toml        spend caps (committed, compiled into the gateway)
web/                       Vite + Svelte 5 + @xyflow/svelte, served as Workers static assets
docs/                      architecture, contracts, ADRs, roadmap, deploy
```

## Commands
```
make check          # fmt check + clippy -D warnings + tests (run before every commit)
make test           # cargo test --workspace (native; no Cloudflare or DB needed)
make build-workers  # worker-build --release for both Workers (needs the wasm32 target)
make dev-api | make dev-gateway | make web | make db     # local dev, see docs/deploy.md
make coldstart      # CPU cost of a cold plugin-registry load, as wasm under Node
make deploy         # web build → gateway → api
make migrate TOKEN=<admin token> [API=https://…]
make types          # TS types from plugins/types/*/schema.json
make hash           # sha256 of a token, for CAIRN_TOKENS
```

## Where code goes
- **Logic goes in the libraries, never in `workers/*`.** Worker crates only adapt bindings (Queue, DO stub, Service Binding, fetch) to the traits in `cairn_app::ports`, `cairn_http::HttpTransport`, `cairn_gateway_core::service::LedgerPort` and `ledger::LedgerSql`. If you're writing an `if` that isn't about a binding, it belongs in a library with a native test.
- **Everything must compile to `wasm32-unknown-unknown`:**
  - no tokio runtime and no filesystem in library code
  - no `std::time::Instant`; use `chrono::Utc::now()`
  - no blocking I/O
  - tokio is a dev-dependency only
- **Keep the cold path cheap.** We plan on the Workers Paid plan, but a new isolate's first request should still stay well under 10 ms of CPU. Don't compile JSON Schemas or do other heavy setup at runtime. Do it at build time (see `cairn-plugins-embedded`), and check with `make coldstart`.
- **Durable Object SQLite forbids `BEGIN`.** The ledger relies on the DO handling one request at a time, plus no awaits between the check and the write. Keep it that way.

## Invariants (docs/ARCHITECTURE.md § Invariants)
1. **A capture is never lost after auth.** Resolve purely, enqueue to CAPTURES, reply 202. The consumer is idempotent (Conflict counts as done).
2. Every mutation goes through `Store::apply(Vec<Op>)`. Payload changes go through `cairn_app::items::patch_payload`, which validates and recomputes idx/title.
3. The core never interprets `item.payload`. It reads only the plugin-declared `idx` fields.
4. Layout lives on `View`, never on `Item`.
5. **Only `workers/gateway` binds a spend credential, and only `cairn-gateway-core` talks to the LLM upstream.**
   - In production the OpenRouter key lives in Cloudflare Secrets Store (AI Gateway BYOK), and the gateway binds only `CF_AIG_TOKEN`. `OPENROUTER_API_KEY` is for local dev only. Never bind both: the gateway refuses.
   - CI greps for `openrouter.ai`, `gateway.ai.cloudflare.com` and `cf-aig-authorization` outside those two crates.
   - The API Worker refuses to serve if it can see either credential.
6. **Code can only tighten spend limits.** Never add endpoints, bindings or vars that raise caps, re-enable the gateway, or give the API Worker access to the Ledger DO. Caps live in `config/gateway.toml`, and lifting a disable requires a deploy with a later `ENABLE_AFTER`.
7. No LLM in orchestration. The Scheduler DO and TASKS consumer are deterministic, and runs can't create tasks or runs.
8. AI output → JSON Schema → Rust `ai_apply` converter → whitelisted ops → core validation. Never apply model output directly.
9. **Nothing polls.** Scheduling uses the Scheduler DO alarm plus `rearm` on writes. Live updates come from the Hub DO. Don't add minute loops.
10. GC purges only trashed items past retention, never pinned items or items tagged `keep`, and always cascades.

If a change needs to break one of these, write an ADR in `docs/adr/` first and ask.

## Conventions
- Rust 2024 edition, stable toolchain. Use `thiserror` in libraries. No `unwrap()` outside tests (clippy enforces this).
- IDs are ULIDs (`cairn_domain::Id`). Relative times like `-24h` and `+2d` go through `cairn_domain::reltime`.
- Error responses use `{ "error": { "code": "snake_case", "message": "…" } }`.
- New item type: add `plugins/types/<name>/` with `plugin.json`, `schema.json` and `views.js`, then run `make types`. It's embedded automatically on the next build. Add a fixture test in `cairn-plugins`.
- New AI capability: add `plugins/ai/<name>/` and a converter in `cairn-app/src/ai_apply.rs` with an explicit allowed-op list, plus tests for invalid output.
- Tests: use `cairn_app::testing::harness()` for app flows (MemStore + in-memory ports), `FakeTransport` for anything HTTP, and `ledger::sqlite::MemSql` for the ledger.
- Update `docs/` whenever you change a contract (plugin manifest, task payload, gateway caps, queue message shapes).

## Secrets and config
- **Secrets are only ever Worker secrets** (`wrangler secret put`), or `.dev.vars` locally (gitignored). Never put them in `config/*.toml`, `wrangler.toml` or code.
- API Worker secrets: `CAIRN_TOKENS`, `SURREAL_USER`, `SURREAL_PASS`, `DISCORD_PUBLIC_KEY`, `DISCORD_BOT_TOKEN`.
- Gateway Worker secret: `CF_AIG_TOKEN` (production, AI Gateway BYOK) or `OPENROUTER_API_KEY` (local dev only), never both. Vars: `AIG_BASE_URL`, `AIG_BYOK_ALIAS`, `ENABLE_AFTER`.
