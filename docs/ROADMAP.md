# Roadmap

Each milestone should be usable on its own before the next one starts. Tick items off as they land.

## M0: Skeleton (this commit)
- [x] Cloud-native layout: API Worker + gateway Worker, Durable Objects (Scheduler, Hub, Ledger), Queues (CAPTURES, TASKS, DLQs), static assets
- [x] Platform-agnostic libraries, unit-tested natively: domain, store (`MemStore`, `SurrealHttpStore`), plugins, scheduler, GC, app, gateway-core
- [x] Capture → Queue → idempotent consumer, with pure resolution (prefix matchers, inbox fallback)
- [x] Scheduler DO: one alarm set to the next reminder or cron task, Discord reminders with action buttons, quiet hours, hourly backstop
- [x] Gateway: compiled-in caps, Ledger DO reserve/settle, fixed routing, hourly key check, breaker, disable-only switch with `ENABLE_AFTER` deploy re-enable, reconcile endpoint
- [x] Web: inbox, canvas (Svelte Flow), plugin `views.js` loader, live refresh through the Hub

## M1: First deploy (phone → stored item, in the cloud)
- [x] **Run everything against a live SurrealDB** (local 4.0.1 so far). `crates/cairn-store/tests/live_surreal.rs` (`make test-live`) runs the whole `Store` contract against a real server: migrations, the `/rpc` envelope, the `surreal-ns` / `surreal-db` headers, `type::record`, `record::id`, `INSERT RELATION`, the unique edge index, `FULLTEXT`, the `op_append_only` event, and purge cascades. Fixes it forced (ADR 0011 amended):
  - **The wire format is now CBOR.** JSON-RPC parsed strings like `"note:abc"` into record ids, and RFC 3339 strings into datetimes, nested values included. That broke `op.target` and would have silently retyped payloads and raw captures.
  - `option<…>` fields reject NULL, so the store omits keys instead of sending null.
  - Datetimes are stored as strings, so `idx` datetimes and `where` values now use one canonical fixed-width UTC format.
  - Ordering filters no longer match missing fields: SurrealQL sorts NONE first, and `MemStore` didn't match them.
  - Namespace and database must exist up front: `make db-init` locally.
- [ ] Run `make test-live` against **Surreal Cloud**, and on Earth specifically, check that table events (`op_append_only`) run.
- [x] **Build the Workers for `wasm32-unknown-unknown`** (`make build-workers`). Both build with `worker-build --release` and no code changes. Sizes: API 5.8 MB raw / 1.57 MB gzipped, gateway 1.1 MB / 0.35 MB.
- [x] **Keep the cold path under the CPU limit** (ADR 0013). Free allows 10 ms of CPU per request, and runtime JSON Schema compilation alone took 30–45 ms on a new isolate's first request. Schemas are now compiled to Rust at build time (`cairn-plugins-embedded`), which brings the cold registry load to 3–4.5 ms (`make coldstart`). **Plan on the Workers Paid plan** anyway: see deploy.md § Plan and CPU limits.
- [ ] Check real CPU time per invocation in Workers Logs after the first deploy (cold capture, cold Discord interaction, queue batch). If queue batches are heavy, consider sending messages as JSON text to skip the `serde-wasm-bindgen` conversion (that's a queue message contract change, so update docs/ingest.md).
- [x] `wrangler dev` end to end (wrangler 4.168, local SurrealDB 4.0.1): `make migrate` (idempotent), then REST capture → CAPTURES queue → consumer → SurrealDB → `GET /api/v1/items/{id}` and `/api/v1/query`, plus an `item.created` event on a `/hub` WebSocket. A bad `?token=` gets 401, and the static web app and SPA fallback are served. The browser UI itself hasn't been clicked through yet.
- [ ] **Check AI Gateway BYOK with OpenRouter.** Cloudflare's BYOK docs don't list supported providers. Things to check:
  - whether the stored key is injected for `/openrouter/...` passthrough
  - whether `AIG_BASE_URL` needs a trailing `/v1`
  - whether `cf-aig-metadata` reaches the analytics and spend-limit rules
  - whether `GET …/key` is proxied with the stored key
  - If `/key` isn't proxied, set `key_check = "best-effort"` (after setting both dashboard limits). Reconcile then reports `ok: null`; a later option is reconciling against AI Gateway analytics via a read-only Cloudflare API token.
- [ ] API tokens in the `api_token` table (replacing the `CAIRN_TOKENS` secret), plus admin endpoints to create and revoke them
- [ ] Webhook JSONPath mapping (`[[webhook]].map`)
- [ ] iOS Shortcut documented and working

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
- Cloudflare Access JWT verification in the API Worker (defense in depth)
- Optional always-on Discord gateway client for free-text DMs
- Embeddings and semantic search (budget roughly 3–6 KB per item)

## Open questions
- Earth or a dedicated Cloud node, once table events and active-hours behaviour are verified
- Lazy versus eager plugin migrations once there are a lot of items
- Calendar: does Cairn become the source of truth, or mirror an external calendar?
