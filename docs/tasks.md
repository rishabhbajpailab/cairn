# Tasks (deterministic orchestration)

A **task** is an item of type `task`. You write it yourself. It declares what runs, when it runs, on which input, with which models and how much it may spend. Each firing creates a **task-run** item. Because tasks, runs and their outputs are all items, the whole pipeline is visible and editable on the canvas.

```
[task: bucket-summaries] ──after──▶ [task: nightly-digest] ──after──▶ [task: reminder-planner]
          ▲ produced-by                     ▲ produced-by
   [task-run 2026-11-23]             [task-run 2026-11-23] ◀─produced-by─ [digest item]
```

## Task payload

YAML is the import/export format (admin endpoints in M5). The item payload is the source of truth.

```yaml
name: nightly-digest
uses: ai/digest                 # AI capability plugin, or builtin/<name> for non-AI jobs
enabled: true
trigger:
  cron: "0 21 * * *"            # evaluated in the timezone set in config/cairn.toml
  # on: { event: item.created, where: { types: [capture] } }
  # after: [bucket-summaries]   # runs once all listed tasks succeed for the same window
input:
  query: { updated_since: "-24h", lifecycle: [active] }
  max_items: 200
models:                         # the best-fit list, which you write yourself. Pin exact slugs.
  - anthropic/claude-haiku-4.5
  - google/gemini-2.5-flash
select: cheapest                # cheapest | first-available
provider:                       # can only TIGHTEN routing (see below)
  zdr: true                     # optional: Zero Data Retention endpoints only
  allow_fallbacks: false        # optional
fanout: { by: bucket, max: 12, concurrency: 4 }   # optional
budget: { per_run_usd: 0.05, per_day_usd: 0.10 }
max_tokens: 2000
on_failure: skip-and-notify     # never falls back to models outside the list
apply: review                   # auto | review
retain_runs: 30d                # GC policy for this task's task-run items
```

## How tasks run

- **Scheduler Durable Object.** It keeps one alarm set to the earliest upcoming cron fire time (or pending reminder). When the alarm fires, it puts a `TaskFire { task, at }` on the **TASKS queue** for each cron firing in `(last_tick, now]`, then re-arms. There's no polling.
- **`after:` chaining.** When a run succeeds, the runner enqueues fires for its dependents (M5).
- **Event triggers** (`on: item.created`). The capture consumer enqueues fires for matching tasks (M5).
- **TASKS consumer** (M5):
  1. computes the run key, and skips the fire if a run with that key already succeeded
  2. resolves the input query
  3. splits it into chunks according to `fanout` and calls the gateway once per chunk, at most `concurrency` at a time
  4. validates the output and converts it to ops
  5. applies the ops or queues them for review

## Rules the scheduler enforces

- **No dynamic spawning.** A run cannot create tasks or runs. `fanout` is the only way to get parallelism, and it is capped by `max`.
- **DAG lint before saving.** The scheduler checks every task item before it is written:
  - `after:` must form an acyclic graph
  - every `uses` must resolve to a loaded plugin
  - every model in `models` must be on the gateway allowlist
  - the worst-case daily cost, summed over the graph, must be within the gateway's daily cap

  A task item that fails lint is saved with `enabled: false` and an error on the item.
- **Idempotency.** Each run is keyed on `hash(task_id, task_version, trigger_window, input_hash)`. If a run with the same key already succeeded, the firing does nothing, so retrying after a crash never charges twice or creates duplicate reminders.
- **`select: cheapest`** re-ranks *only* the listed models. It uses the gateway's cached OpenRouter price table, weighted by the capability's typical input-to-output token ratio.
- **Provider routing is fixed by the gateway.** Every request is sent with `sort: price`, `data_collection: deny`, `require_parameters: true`, and a `max_price` taken from the allowlist. A task's `provider` block can only add `zdr: true` or `allow_fallbacks: false`, and the gateway ignores any other key.
- **Dry run.** `POST /api/v1/tasks/{id}/dry-run` resolves the trigger window, the input set, the chunking, the chosen model and the worst-case cost, and returns them without calling the gateway.

## Built-in, non-AI tasks

| `uses` | What it does |
|---|---|
| `builtin/gc` | Runs the lifecycle policies (see [lifecycle-gc.md](lifecycle-gc.md)) |
| `builtin/notify` | Sends due reminders through Discord (the Scheduler DO alarm does this automatically, so you rarely need it as a task) |
| `builtin/export` | Writes a nightly JSON and Markdown export of the graph to an R2 bucket, so a copy exists outside Surreal |
| `builtin/reconcile` | Calls gateway `POST /v1/reconcile`, which disables the gateway if the ledger and OpenRouter usage don't match |
