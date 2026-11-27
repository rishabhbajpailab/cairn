# Cairn: architecture

Cairn is a cloud-native "second brain". It captures anything from anywhere (REST, webhooks, Discord, scripts) into one typed graph of items, lets you organize that graph visually, and runs deterministic, budget-capped AI tasks over it.

It runs on **a VPS, with Docker Compose** (ADR 0014), next to Engine of Creation and using that host's **SurrealDB 4.x**, with **OpenRouter** for models, reached through **Cloudflare AI Gateway** with the key stored there (BYOK). Everything from outside passes Cairn's own GitHub sign-in (`cairn-sign-in`, oauth2-proxy) except the machine routes, which check their own tokens (ADR 0015). It started on Cloudflare Workers. `workers/*` still builds and is kept until the VPS deploy has run for a while.

> The name is a placeholder. Renaming means a find-and-replace on `cairn`.

## Components

```
                    ┌────────────────────────── cairn (container) ───────────────────────────┐
 iOS Shortcut ─────▶│ /api/v1/capture ──┐                                                    │
 scripts ──────────▶│ /ingest/webhook/* ├─▶ resolve (pure) ─▶ SQLite spool (fsync) ──┐       │
 Discord ──────────▶│ /integrations/discord/interactions ┘     /data volume            │       │
 Browser ──────────▶│ /api/v1/* (items, edges, query)       spool consumer ◀──────────┘       │
 (door → sign-in)   │ /hub ──▶ in-process broadcast (WebSockets)  │ persist + notify          │
        ▲  ws       │                                             ▼                            │
        └───────────│ scheduler timer: sleeps until earliest reminder / cron task ─▶ spool    │
                    │ web app + plugins/types/*/views.js from /app/web                         │
                    └───────┬────────────────────────────────────────────┬────────────────────┘
                            │ HTTP /rpc, CBOR (database-level user)       │ private network "gateway"
                            ▼                                             ▼
                    ┌────────────────────┐           ┌──────── cairn-gateway (container) ──────┐
                    │ EoC's `index`      │           │ only holder of a spend credential        │
                    │ (SurrealDB 4.0.1;  │           │ no published port · caps compiled in     │
                    │  namespace cairn)  │           │ ledger: SQLite on its own volume         │
                    └────────────────────┘           └───────────────────┬──────────────────────┘
                                                                         │ cf-aig-authorization (no provider key)
                                                                         ▼
                                                   Cloudflare AI Gateway: OpenRouter key in Secrets Store (BYOK)
                                                     logs · analytics · spend-limit rules
                                                                         ▼
                                                                    OpenRouter
```

| Crate | Role | Runs |
|---|---|---|
| `cairn-domain` | Pure types: `Item`, `Edge`, `View`, `Op`, `Capture`, `Query`, relative times | everywhere |
| `cairn-http` | `HttpTransport` trait, a fake for tests, and a reqwest transport (feature `reqwest`, hosts only) | everywhere |
| `cairn-store` | `Store` trait, `MemStore` for tests, `SurrealHttpStore` (SurrealDB `/rpc` over HTTP) | everywhere |
| `cairn-plugins` | Plugin registry (from the filesystem, or embedded in the build), JSON Schema validation, idx extraction, capture matchers, migrations | everywhere |
| `cairn-plugins-embedded` | The repo's `plugins/` embedded in the binary, with each JSON Schema compiled to Rust at build time (no schema compilation in the request path) | inside `cairn` (and the API Worker), plus native tests |
| `cairn-scheduler` | Task definitions, DAG lint, cron evaluation, `next_fire_after`, run keys | everywhere |
| `cairn-gc` | Lifecycle policies as a pure planner | everywhere |
| `cairn-app` | Platform-agnostic application: router, ingest, spool consumer body, reminders, Discord, AI apply. Talks to the platform only through the traits in `ports.rs`. | inside `cairn` (and the API Worker), plus native tests |
| `cairn-gateway-core` | Caps, ledger (over a SQL trait), model selection, OpenRouter client (AI Gateway BYOK or direct), request flow | inside `cairn-gateway` (and the gateway Worker), plus native tests |
| `hosts/server` | Binary `cairn` (VPS): bindings-free glue. Axum on tokio, SQLite spool + consumer, scheduler timer, hub broadcast, static web, `migrate` / `spool` commands | VPS (container) |
| `hosts/gateway` | Binary `cairn-gateway` (VPS): the gateway routes over a SQLite ledger file. Only process with a spend credential | VPS (container) |
| `workers/api` | Glue: bindings → ports, `fetch` / `queue` / `scheduled` entry points, Scheduler and Hub Durable Objects | Workers (wasm32), legacy |
| `workers/gateway` | Glue: Ledger Durable Object, Service Binding entry point, fetch transport | Workers (wasm32), legacy |

**Design rule:** all logic lives in the libraries and is unit-tested natively against `MemStore`, `FakeTransport` and the in-memory ports. The host crates (`hosts/*`, and the legacy `workers/*`) only translate between the platform (tokio, SQLite, HTTP, or Cloudflare bindings) and those traits. The exception is the host infrastructure itself, such as the spool's retry bookkeeping, which is tested natively in `hosts/server`.

## Invariants

Everything else depends on these. Breaking one needs an ADR.

1. **A capture is never lost once auth passes.** It's durable as soon as it's committed to the SQLite spool (fsynced, on a volume), before the 202. That holds even if Surreal is down or the process restarts. The consumer retries with backoff, and anything that still fails is marked dead and kept (`cairn spool status`, `cairn spool retry-dead`).
2. **One graph.** Buckets, tasks, task runs and reminders are all items. Membership, chaining and provenance are all edges.
3. **Layout belongs to views, not items.**
4. **Every mutation is an op.** `Store::apply(ops)` sends a single transaction that also writes the ops log. A table event in Surreal blocks deletes and rewrites of the ops log.
5. **Only the gateway holds a spend credential.**
   - The OpenRouter key lives in Cloudflare **Secrets Store**, attached to AI Gateway (BYOK). Cairn never sees it.
   - The `cairn-gateway` container alone holds `CF_AIG_TOKEN` (`deploy/gateway.env`), which authorizes requests to AI Gateway.
   - `cairn` refuses to start if it can see either credential, and `cairn-gateway` refuses if it can see the Surreal credentials or `CAIRN_TOKENS`.
   - The gateway has no published port, and shares a private network only with `cairn`.
6. **Code can only tighten spend limits.** Caps are compiled into the gateway. There's a disable endpoint and no enable endpoint. Re-enabling means an operator sets a newer `ENABLE_AFTER` in the gateway's env file and recreates its container.
7. **Orchestration is deterministic.** Task items declare their trigger, input, models and budget. The scheduler evaluates them with no LLM involved.
8. **AI proposes and the core decides.** Output is validated against its JSON Schema, then converted to ops by a Rust converter, then those ops are checked against a whitelist and validated before anything is applied.
9. **Nothing polls.** The scheduler sleeps until the next due event (an hourly backstop at most) and is woken by `rearm`. The spool consumer sleeps until notified or until a retry is due. Live updates are pushed through the hub, and Surreal is only touched when there's work to do.

## Data flow: capture to item

1. A route authenticates the request (scoped token, webhook token or Discord signature).
2. **Pure resolution**: an explicit type, then plugin prefix matchers, then the Inbox fallback (`hold-that-thought`, tagged `unsorted`). The item id is decided at this point.
3. `CaptureMsg { capture, item }` is committed to the **spool**, and the reply is sent (`202`, with the planned type, so Discord can say "Saved as todo").
4. The **spool consumer** stores the raw capture and creates the item. On redelivery the same ids produce Conflict, which counts as already done.
5. The **Hub** is notified (open browsers refetch). For reminders and tasks, the **Scheduler** is re-armed.

## Data flow: scheduling

- The **scheduler timer** in `cairn` sleeps until `next_wake`: the earliest of every pending reminder's `max(fire_at, snoozed_until)` (pushed past quiet hours) and every enabled task's next cron time. It never sleeps longer than an hour (the backstop).
- When it wakes on time it:
  1. sends due reminders as Discord DMs with action buttons, and marks them `sent`
  2. puts cron firings for `(last_tick, now]` on the spool's **tasks** queue (`last_tick` is kept in the spool database, so it survives restarts)
  3. computes the next wake
- Any write to a reminder or task item calls `rearm`, which wakes the timer to recompute.
- Ticks and spool deliveries are at-least-once. Reminder state and task run keys make a duplicate firing harmless.

See also [data-model.md](data-model.md), [plugins.md](plugins.md), [tasks.md](tasks.md), [spend-control.md](spend-control.md), [lifecycle-gc.md](lifecycle-gc.md), [ingest.md](ingest.md), [deploy.md](deploy.md) and [ROADMAP.md](ROADMAP.md).
