# Roadmap

Each milestone should be usable on its own before the next one starts. Tick items off as they land.

## M0: Skeleton (this commit)
- [x] Cloud-native layout: API Worker + gateway Worker, Durable Objects (Scheduler, Hub, Ledger), Queues (CAPTURES, TASKS, DLQs), static assets
- [x] Platform-agnostic libraries, unit-tested natively: domain, store (`MemStore`, `SurrealHttpStore`), plugins, scheduler, GC, app, gateway-core
- [x] Capture → Queue → idempotent consumer, with pure resolution (prefix matchers, inbox fallback)
- [x] Scheduler DO: one alarm set to the next reminder or cron task, Discord reminders with action buttons, quiet hours, hourly backstop
- [x] Gateway: compiled-in caps, Ledger DO reserve/settle, fixed routing, hourly key check, breaker, disable-only switch with `ENABLE_AFTER` deploy re-enable, reconcile endpoint
- [x] Web: inbox, canvas (Svelte Flow), plugin `views.js` loader, live refresh through the Hub

## M1: First deploy (phone → stored item, on the VPS)
- [x] **Move to the VPS** (ADR 0014). `hosts/server` (`cairn`) and `hosts/gateway` (`cairn-gateway`) are native binaries over the same libraries. Captures and task fires go through a durable SQLite spool, the scheduler is a tokio timer, the hub is an in-process broadcast, and the gateway ledger is a SQLite file. Docker Compose has two containers, and the gateway sits on a private network with no published port. Verified natively against SurrealDB 4.0.1, simulating the VPS (EoC data in another namespace, Cairn on a database-level user):
  - migrate (idempotent) → capture → spool → SurrealDB → `item.created` on `/hub`; a bad `?token=` gets 401
  - **SurrealDB outage plus a `cairn` restart:** 3 captures accepted with 202 while the DB was down, still pending after the restart, delivered about 11 s after the DB came back
  - the `cairn` user can't log in to EoC's namespace; each process refuses to start with the other one's secrets
  - Fixed on the way: database-level users need the `surreal-auth-ns` / `surreal-auth-db` headers (this would also have broken Surreal Cloud). The store also treated a plain-text 401 as an empty success, so migrate "succeeded" without running. Both are fixed, with tests.
- [x] **Share EoC's SurrealDB and door** (ADR 0014 amendment): two narrow networks, never EoC's `inside`.
- [x] **GitHub sign-in in front of Cairn** (ADR 0015): Cairn's own OAuth App, through oauth2-proxy v7.15.6 (EoC's version). Machine routes are let through to Cairn's token checks, and a test keeps `deploy/sign-in.cfg` equal to `MACHINE_ROUTES`. Tried against oauth2-proxy built from source, without a real GitHub login.
- [x] **Weekly backups to S3/R2** (ADR 0015): `deploy/backup.sh` from cron. Back up, wipe, re-init and restore were tried end to end (SurrealDB 4.0.1, rclone 1.68.2 against its local backend).
- [ ] **First real VPS deploy:** EoC up with its Cairn changes, `deploy/surreal-init.surql`, the OAuth App, env files and secrets, `make vps-up`, `make vps-migrate`, `sites/cairn.caddy` (docs/deploy.md). CI builds the images on every push and pushes them to GHCR on a version tag (`v0.1.0` is the first); the VPS only pulls.
- [ ] After that deploy: a real GitHub sign-in, live updates (`/hub`) through the sign-in, one real backup upload to the bucket, and the door starting with and without `sites/cairn.caddy`.
- [x] **Run everything against a live SurrealDB** (4.0.1). `crates/cairn-store/tests/live_surreal.rs` (`make test-live`) runs the whole `Store` contract against a real server: migrations, the `/rpc` envelope, the `surreal-ns` / `surreal-db` headers, `type::record`, `record::id`, `INSERT RELATION`, the unique edge index, `FULLTEXT`, the `op_append_only` event, and purge cascades. Fixes it forced (ADR 0011 amended):
  - **The wire format is now CBOR.** JSON-RPC parsed strings like `"note:abc"` into record ids, and RFC 3339 strings into datetimes, nested values included. That broke `op.target` and would have silently retyped payloads and raw captures.
  - `option<…>` fields reject NULL, so the store omits keys instead of sending null.
  - Datetimes are stored as strings, so `idx` datetimes and `where` values now use one canonical fixed-width UTC format.
  - Ordering filters no longer match missing fields: SurrealQL sorts NONE first, and `MemStore` didn't match them.
  - Namespace and database must exist up front: `make db-init` locally.
- [ ] Browser click-through of the web UI served by `cairn` (inbox, canvas, live refresh).
- [ ] **Check AI Gateway BYOK with OpenRouter.** Cloudflare docs (checked 2026-11-26) confirm the design. The live checks still need a deploy:
  - [x] Stored keys are injected on provider passthrough (`/openrouter/...`) only when no provider `Authorization` header is sent. The gateway sends none, and a test asserts that.
  - [x] `cf-aig-byok-alias` selects a non-default stored key on passthrough requests (the alias is ignored only on Unified Billing endpoints, which Cairn doesn't use).
  - [x] Spend-limit rules and analytics can filter and split by custom metadata (`cf-aig-metadata`). Spend limits are eventually consistent, so they remain the outer net and the ledger stays primary.
  - [x] Added `cf-aig-no-wholesale: true`. Without it, a missing or mistyped stored key could fall back to Unified Billing, charging Cloudflare credits and bypassing the OpenRouter key limit. It now fails with error 2044.
  - [ ] Trailing `/v1`: the OpenRouter page shows both `…/openrouter/chat/completions` and `…/openrouter/v1/chat/completions`. `AIG_BASE_URL` uses `/v1`; confirm with one real call.
  - [ ] Whether `GET …/openrouter/v1/key` is proxied with the stored key (not documented). If not, set `key_check = "best-effort"` (after setting both dashboard limits). Reconcile then reports `ok: null`. A later option is reconciling against AI Gateway analytics (GraphQL `aiGatewayRequestsAdaptiveGroups`, `sum { cost }`) via a read-only Account Analytics token.
  - [ ] Optionally also turn on **Require provider credentials** on the gateway, which enforces the same thing as `cf-aig-no-wholesale` on the dashboard side.
- [ ] API tokens in the `api_token` table (replacing `CAIRN_TOKENS`), plus admin endpoints to create and revoke them
- [ ] Webhook JSONPath mapping (`[[webhook]].map`)
- [ ] iOS Shortcut documented and working

### Done on the way, for the legacy Workers path
- [x] **Build the Workers for `wasm32-unknown-unknown`** (`make build-workers`). Both build with `worker-build --release` and no code changes. Sizes: API 5.8 MB raw / 1.57 MB gzipped, gateway 1.1 MB / 0.35 MB.
- [x] **Keep the cold path under the CPU limit** (ADR 0013). Free allows 10 ms of CPU per request, and runtime JSON Schema compilation alone took 30–45 ms on a new isolate's first request. Schemas are now compiled to Rust at build time (`cairn-plugins-embedded`), which brings the cold registry load to 3–4.5 ms (`make coldstart`). On the VPS there is no per-request CPU limit, so this is now a startup nicety.
- [x] `wrangler dev` end to end (wrangler 4.168, local SurrealDB 4.0.1): `make migrate` (idempotent), then REST capture → CAPTURES queue → consumer → SurrealDB → `GET /api/v1/items/{id}` and `/api/v1/query`, plus an `item.created` event on a `/hub` WebSocket. A bad `?token=` gets 401, and the static web app and SPA fallback are served. The browser UI itself hasn't been clicked through yet.

## M2: See it (web)
- [ ] Table view with filters (tags, type, indexed fields)
- [ ] Richer `todo`, `hold-that-thought` and `bucket` renderers; type editors
- [ ] Item detail view; archive, trash and restore in the UI
- [ ] Bulk plugin migration endpoint (`/api/v1/admin/migrate-type/{type}`)

## M3: Arrange it (canvas)
- [ ] Persist canvas layout per view (`ViewLayout` ops) instead of local positions
- [ ] Drag to link (choose the edge kind), drag onto a bucket for `contains`
- [ ] Board view: buckets as columns
- [ ] Undo/redo from the ops log (capture `before` in `apply`)

## M4: Discord
- [ ] Component buttons → plugin actions (`act:<id>:<action>`)
- [ ] `/remind` with natural-language time parsing (deterministic parser first)
- [ ] `/find`
- [ ] Morning digest DM

## M5: Tasks and AI
- [ ] TASKS consumer: run keys, input resolution, fan-out, gateway calls, apply or review
- [ ] DAG lint on save; dry-run endpoint
- [ ] `ai/triage` (on-ingest) and `ai/digest` (nightly), with `ai_apply` converters
- [ ] Review queue for `suggested` items and edges (canvas + Discord)
- [ ] `builtin/reconcile` → gateway `/v1/reconcile`

## M6: Lifecycle
- [ ] `builtin/gc` with policies, dry run and purge cascade
- [ ] `nudge` action in the digest
- [ ] `builtin/export` to R2 (JSON + Markdown)
- [ ] Tiered condensation (working → memory → core): a `tier` field plus `ai/condense` and `ai/distill` tasks with fan-out by bucket

## Later
- More types: `calendar-entry` (with CalDAV sync hooks), `research`, `idea`, `link`
- Delete `workers/*` once the VPS deploy has run for a while (ADR 0014)
- An auth layer in front of the web UI (e.g. the reverse proxy's forward auth), now that Cloudflare Access doesn't sit there
- Optional always-on Discord gateway client for free-text DMs (easy now: a background task in `cairn`)
- Embeddings and semantic search (budget roughly 3–6 KB per item)

## Open questions
- Lazy versus eager plugin migrations once there are a lot of items
- Calendar: does Cairn become the source of truth, or mirror an external calendar?
