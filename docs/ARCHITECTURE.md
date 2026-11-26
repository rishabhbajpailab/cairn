# Cairn: architecture

Cairn is a cloud-native "second brain". It captures anything from anywhere (REST, webhooks, Discord, scripts) into one typed graph of items, lets you organize that graph visually, and runs deterministic, budget-capped AI tasks over it.

It runs entirely on **Cloudflare Workers + Durable Objects + Queues**, with **SurrealDB on Surreal Cloud** as the database and **OpenRouter** for models. Nothing runs on your own hardware.

> The name is a placeholder. Renaming means a find-and-replace on `cairn`.

## Components

```
                    ┌──────────────────────── cairn-api (Worker) ─────────────────────────┐
 iOS Shortcut ─────▶│ /api/v1/capture ──┐                                                │
 scripts ──────────▶│ /ingest/webhook/* ├─▶ resolve (pure) ─▶ CAPTURES queue ──┐          │
 Discord ──────────▶│ /integrations/discord/interactions ┘                       │          │
 Browser (Access) ─▶│ /api/v1/* (items, edges, query)      queue consumer ◀──────┘          │
        ▲  ws       │ /hub ─────────────▶ Hub DO (WebSocket fan-out)  │ persist + notify     │
        └───────────│                                                 ▼                      │
                    │ Scheduler DO: one alarm = earliest reminder / cron task ──▶ TASKS queue │
                    │ hourly cron: re-arm Scheduler (backstop)                               │
                    │ static assets: web app + plugins/types/*/views.js                      │
                    └───────┬───────────────────────────────────────────┬────────────────────┘
                            │ HTTPS /rpc (stateless)                     │ Service Binding (private)
                            ▼                                            ▼
                    ┌───────────────┐                  ┌──────── cairn-gateway (Worker) ────────┐
                    │ Surreal Cloud │                  │ only holder of a spend credential       │
                    │ (SurrealDB)   │                  │ no public URL · caps compiled in        │
                    └───────────────┘                  │ Ledger DO (SQLite): reserve → settle    │
                                                       └───────────────────┬─────────────────────┘
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
| `cairn-http` | `HttpTransport` trait plus a fake for tests | everywhere |
| `cairn-store` | `Store` trait, `MemStore` for tests, `SurrealHttpStore` (SurrealDB `/rpc` over HTTP) | everywhere |
| `cairn-plugins` | Plugin registry (from the filesystem, or embedded in the build), JSON Schema validation, idx extraction, capture matchers, migrations | everywhere |
| `cairn-scheduler` | Task definitions, DAG lint, cron evaluation, `next_fire_after`, run keys | everywhere |
| `cairn-gc` | Lifecycle policies as a pure planner | everywhere |
| `cairn-app` | Platform-agnostic application: router, ingest, Queue consumer body, reminders, Discord, AI apply. Talks to the platform only through the traits in `ports.rs`. | inside the API Worker, plus native tests |
| `cairn-gateway-core` | Caps, ledger (over a SQL trait), model selection, OpenRouter client (AI Gateway BYOK or direct), request flow | inside the gateway Worker, plus native tests |
| `workers/api` | Glue: bindings → ports, `fetch` / `queue` / `scheduled` entry points, Scheduler and Hub Durable Objects | Workers (wasm32) |
| `workers/gateway` | Glue: Ledger Durable Object, Service Binding entry point, fetch transport | Workers (wasm32) |

**Design rule:** all logic lives in the libraries and is unit-tested natively against `MemStore`, `FakeTransport` and the in-memory ports. The `workers/*` crates only translate between Cloudflare bindings and those traits.

## Invariants

Everything else depends on these. Breaking one needs an ADR.

1. **A capture is never lost once auth passes.** It's durable as soon as the CAPTURES Queue accepts it, even if Surreal is paused or unreachable. The consumer retries, and anything that still fails goes to a dead-letter queue.
2. **One graph.** Buckets, tasks, task runs and reminders are all items. Membership, chaining and provenance are all edges.
3. **Layout belongs to views, not items.**
4. **Every mutation is an op.** `Store::apply(ops)` sends a single transaction that also writes the ops log. A table event in Surreal blocks deletes and rewrites of the ops log.
5. **Only the gateway holds a spend credential.**
   - The OpenRouter key lives in Cloudflare **Secrets Store**, attached to AI Gateway (BYOK). No Worker ever sees it.
   - The gateway Worker alone holds `CF_AIG_TOKEN`, which authorizes requests to AI Gateway.
   - The API Worker refuses to serve if it can see either credential.
   - The gateway has no public URL.
6. **Code can only tighten spend limits.** Caps are compiled into the gateway. There's a disable endpoint and no enable endpoint, and re-enabling requires a deploy with a newer `ENABLE_AFTER`.
7. **Orchestration is deterministic.** Task items declare their trigger, input, models and budget. The Scheduler DO evaluates them with no LLM involved.
8. **AI proposes and the core decides.** Output is validated against its JSON Schema, then converted to ops by a Rust converter, then those ops are checked against a whitelist and validated before anything is applied.
9. **Nothing polls.** Scheduling is a single Durable Object alarm, live updates are pushed through the Hub, and Surreal is only touched when there's work to do.

## Data flow: capture to item

1. A Worker route authenticates the request (scoped token, webhook token or Discord signature).
2. **Pure resolution**: an explicit type, then plugin prefix matchers, then the Inbox fallback (`hold-that-thought`, tagged `unsorted`). The item id is decided at this point.
3. `CaptureMsg { capture, item }` goes onto the **CAPTURES queue**, and the reply is sent (`202`, with the planned type, so Discord can say "Saved as todo").
4. The **queue consumer** stores the raw capture and creates the item. On redelivery the same ids produce Conflict, which counts as already done.
5. The **Hub** is notified (open browsers refetch). For reminders and tasks, the **Scheduler** is re-armed.

## Data flow: scheduling

- The **Scheduler DO** keeps a single alarm set to `next_wake`: the earliest of every pending reminder's `max(fire_at, snoozed_until)` (pushed past quiet hours) and every enabled task's next cron time.
- When the alarm fires it:
  1. sends due reminders as Discord DMs with action buttons, and marks them `sent`
  2. puts cron firings for `(last_tick, now]` on the **TASKS queue**
  3. re-arms the alarm
- Any write to a reminder or task item re-arms it. An hourly Cron Trigger is the backstop.
- Alarms and queues are at-least-once. Reminder state and task run keys make a duplicate firing harmless.

See also [data-model.md](data-model.md), [plugins.md](plugins.md), [tasks.md](tasks.md), [spend-control.md](spend-control.md), [lifecycle-gc.md](lifecycle-gc.md), [ingest.md](ingest.md), [deploy.md](deploy.md) and [ROADMAP.md](ROADMAP.md).
