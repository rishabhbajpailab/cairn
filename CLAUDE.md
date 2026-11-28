# CLAUDE.md

Cairn is a single-user "second brain". It runs on **a VPS with Docker Compose** (ADR 0014), next to Engine of Creation and using that host's **SurrealDB 4.x**, with **OpenRouter** behind an isolated gateway process. The gateway reaches OpenRouter through **Cloudflare AI Gateway**, using a key stored there (BYOK). It captures items from REST, webhooks and Discord into a typed graph, shows them on a visual canvas, and runs deterministic, budget-capped AI tasks over them. Start with `docs/ARCHITECTURE.md`. The current milestone is in `docs/ROADMAP.md` (M1: first VPS deploy). The original Cloudflare Workers glue (`workers/*`) still builds but is legacy.

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
hosts/server               binary `cairn` (VPS): axum/tokio glue, SQLite spool + consumer, scheduler timer, hub, static web
hosts/gateway              binary `cairn-gateway` (VPS): gateway routes over a SQLite ledger; only spend credential
workers/api, workers/gateway  legacy Cloudflare Workers glue (ADR 0008, superseded by 0014)
Dockerfile, compose.yaml   VPS images (targets `cairn`, `gateway`) and services (cairn-sign-in, cairn, cairn-gateway)
deploy/                    env examples, sign-in.cfg (oauth2-proxy), surreal-init.surql, backup.sh (weekly → S3/R2)
plugins/types/*            type plugins (plugin.json, schema.json, views.js)
plugins/ai/*               AI capabilities (plugin.json, prompt.md, output.schema.json)
schema/*.surql             SurrealDB schema, applied in order via POST /api/v1/admin/migrate
config/cairn.toml          app config (committed, embedded at build time; CAIRN_CONFIG overrides on the VPS)
config/gateway.toml        spend caps (committed, compiled into the gateway)
web/                       Vite + Svelte 5 + @xyflow/svelte, served as Workers static assets
docs/                      architecture, contracts, ADRs, roadmap, deploy
```

## Commands
```
make check          # fmt check + clippy -D warnings + tests (run before every commit)
make test           # cargo test --workspace (native; no Cloudflare or DB needed)
make test-live      # SurrealHttpStore against a real SurrealDB (default: local `make db`)
make db | make db-init | make run-gateway | make run-server | make web   # native local dev, see docs/deploy.md
make vps-up | make vps-migrate | make vps-logs                          # on the VPS: pulls GHCR images CI pushes on a vX.Y.Z tag (vps-build builds locally)
make vps-backup | make vps-backup-upload                                # deploy/backup.sh: local copy | + upload (weekly cron)
make build-workers  # legacy: worker-build --release for both Workers (needs the wasm32 target)
make coldstart      # CPU cost of a cold plugin-registry load, as wasm under Node
make migrate TOKEN=<admin token> [API=https://…]   # or `cairn migrate` inside the container
make types          # TS types from plugins/types/*/schema.json
make hash           # sha256 of a token, for CAIRN_TOKENS
```

## Where code goes
- **Logic goes in the libraries, never in `hosts/*` or `workers/*`.** Host crates only adapt the platform (tokio, SQLite, HTTP; or Cloudflare bindings for the legacy Workers) to the traits in `cairn_app::ports`, `cairn_http::HttpTransport`, `cairn_gateway_core::service::LedgerPort` and `ledger::LedgerSql`. If you're writing an `if` that isn't about the platform, it belongs in a library with a native test. Host infrastructure itself (the spool's retry and dead-letter bookkeeping, the scheduler loop) lives in `hosts/server` with its own tests.
- **`hosts/server` must never depend on `cairn-gateway-core`, and `hosts/gateway` must never depend on `cairn-store` or `cairn-app`.** CI checks both.
- **Libraries must still compile to `wasm32-unknown-unknown`** (keeps them portable and the legacy Workers building). Native-only code goes behind a feature that only `hosts/*` enables, like `cairn-http/reqwest` and `cairn-gateway-core/sqlite`:
  - no tokio runtime and no filesystem in library code
  - no `std::time::Instant`; use `chrono::Utc::now()`
  - no blocking I/O
  - tokio is a dev-dependency only
- **Keep startup cheap.** Precompile at build time rather than at runtime (see `cairn-plugins-embedded`, ADR 0013). `make coldstart` measures it.
- **The ledger relies on serialized access with no awaits between check and write.** On the VPS that's one mutex around a synchronous `Ledger` call; on Workers it was the DO, whose SQLite forbids `BEGIN`. Keep `Ledger` methods synchronous.

## Invariants (docs/ARCHITECTURE.md § Invariants)
1. **A capture is never lost after auth.** Resolve purely, commit to the SQLite spool (fsync), reply 202. The consumer is idempotent (Conflict counts as done) and retries; exhausted rows are kept as dead, never dropped.
2. Every mutation goes through `Store::apply(Vec<Op>)`. Payload changes go through `cairn_app::items::patch_payload`, which validates and recomputes idx/title.
3. The core never interprets `item.payload`. It reads only the plugin-declared `idx` fields.
4. Layout lives on `View`, never on `Item`.
5. **Only the gateway (`hosts/gateway`, legacy `workers/gateway`) holds a spend credential, and only `cairn-gateway-core` talks to the LLM upstream.**
   - In production the OpenRouter key lives in Cloudflare Secrets Store (AI Gateway BYOK), and the gateway gets only `CF_AIG_TOKEN` (`deploy/gateway.env`). `OPENROUTER_API_KEY` is for local dev only. Never set both: the gateway refuses.
   - The gateway container has no published port and shares a private network only with `cairn`.
   - CI greps for `openrouter.ai`, `gateway.ai.cloudflare.com` and `cf-aig-authorization` outside the gateway crates.
   - `cairn` refuses to start if it can see either credential. The gateway refuses to start if it can see the Surreal credentials or `CAIRN_TOKENS`.
6. **Code can only tighten spend limits.** Never add endpoints, env vars or mounts that raise caps, re-enable the gateway, or give `cairn` access to the ledger volume or the gateway env. Caps live in `config/gateway.toml` (compiled in). Lifting a disable requires an operator to set a later `ENABLE_AFTER` and recreate the gateway container.
7. No LLM in orchestration. The scheduler timer and tasks consumer are deterministic, and runs can't create tasks or runs.
8. AI output → JSON Schema → Rust `ai_apply` converter → whitelisted ops → core validation. Never apply model output directly.
9. **Nothing polls.** The scheduler sleeps until the next due event (hourly backstop) and `rearm` wakes it. The spool consumer sleeps until notified or a retry is due. Live updates come from the hub. Don't add minute loops.
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
- **Secrets live only in `deploy/*.env` and `deploy/secrets/` on the VPS** (`cairn.env`, `gateway.env`, `sign-in.env`, `backup.env`; gitignored, `chmod 600`), or `.dev.vars` for the legacy Workers. Never put them in `config/*.toml`, `compose.yaml`, the Dockerfile or code.
- `cairn`: `CAIRN_TOKENS`, `SURREAL_USER`, `SURREAL_PASS` (a database-level user, `SURREAL_AUTH_LEVEL=database`), `DISCORD_PUBLIC_KEY`, `DISCORD_BOT_TOKEN`. Non-secret: `SURREAL_URL`, `GATEWAY_URL`, `SPOOL_PATH`, `CAIRN_WEB_DIR`, `CAIRN_BIND`, `CAIRN_CONFIG`.
- `cairn-gateway`: `CF_AIG_TOKEN` (production, AI Gateway BYOK) or `OPENROUTER_API_KEY` (local dev only), never both. Non-secret: `AIG_BASE_URL`, `AIG_BYOK_ALIAS`, `ENABLE_AFTER`, `LEDGER_PATH`, `GATEWAY_BIND`.
- SurrealDB is EoC's `index` container (ADR 0014 amendment). Cairn uses only namespace `cairn` (`deploy/surreal-init.surql`), and never EoC's `eoc` namespace or EoC's root password, except once, by the operator, to run the init script.
- **Cairn joins EoC only over `eoc-cairn-index` (`cairn` only) and `eoc-cairn-door` (`cairn-sign-in` only), in `compose.eoc.yaml`. Never put anything of Cairn's on EoC's `inside` network**, where EoC's `book` has no sign-in of its own, and never put the gateway on any EoC network. CI checks the merged compose config. Changes to EoC's side go in its repository, following its CLAUDE.md (record decisions in its `docs/DECISIONS.md`).
- Cairn's namespace is not rebuildable, unlike EoC's part of the index: keep `deploy/backup.sh` working. Exports skip users (`--users false`: SurrealDB redacts hashes), and restores go through `surreal sql` as Cairn's user in a throwaway container. `history.txt` gets written wherever `surreal sql` runs, so never run it inside `index` or the checkout with a password or a backup.
- **Everything from outside goes through `cairn-sign-in`** (ADR 0015). Only `cairn-sign-in` is on the door's network or published; `cairn` never is. Machine routes, the only ones that bypass the GitHub sign-in, are `cairn_app::api::MACHINE_ROUTES` and `skip_auth_routes` in `deploy/sign-in.cfg`. Change both together; `hosts/server/tests/sign_in_routes.rs` fails if they differ. Keep the list short: each entry must check its own credential.
- Sign-in secrets: `deploy/sign-in.env` (client ID, redirect URL, `OAUTH2_PROXY_GITHUB_USERS`) and `deploy/secrets/` (client secret, cookie secret). Backups: `deploy/backup.env` (rclone S3/R2 settings, a key scoped to one private bucket).
