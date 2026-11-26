# Spend control

**Requirement:** no code path can make AI spend exceed the configured limits. That includes plugins, tasks, bugs and a compromised API Worker.

**Rule:** code can only *tighten* limits. Loosening any limit means a `wrangler deploy` made with your Cloudflare credentials, or a change in a dashboard (Cloudflare or OpenRouter). The app holds no credential for either.

```
API Worker ──Service Binding──▶ gateway Worker ──cf-aig-authorization──▶ AI Gateway ──stored key──▶ OpenRouter
                                (ledger, caps,                          (spend-limit rules,         (key credit
                                 allowlist)                              logs; BYOK key)             limit)
```

There are three independent caps, enforced by three different systems.

## Layer 1: the OpenRouter key (the hard stop)

| Control | Where it's set | Why code can't bypass it |
|---|---|---|
| A **dedicated Cairn key** with a credit limit and reset | OpenRouter dashboard | The key can't change its own limit, and no Worker can even read it (BYOK) |
| **No management key anywhere** | Policy, checked hourly | Management keys can raise limits. The gateway calls `GET /key` and disables itself if the key is a management key or has `limit: null` (see `key_check` below). |
| Small prepaid balance, auto top-up **off** | OpenRouter billing | Money that isn't in the account can't be spent |
| Account-wide provider allowlist | OpenRouter privacy settings | It acts as a ceiling over any per-request `only` list |

Keep Cairn's key separate from other apps, using AI Gateway's `cairn` alias. Then this limit is Cairn's alone, and the nightly reconcile compares like with like.

## Layer 2: Cloudflare AI Gateway (BYOK keystore + outer cap)

- **The OpenRouter key is stored in Secrets Store**, as the gateway's Provider Key with alias `cairn`. AI Gateway injects it only when a request arrives *without* a provider `Authorization` header. The gateway Worker never sends one, so no Worker or log ever holds the raw key.
- **`CF_AIG_TOKEN`** (an auth token for the `cairn` gateway) is the only credential the gateway Worker holds. Treat it as a spend credential: anyone with it can spend through the stored key, up to the limits in this layer and layer 1.
- **A spend-limit rule** sits slightly above `global.per_day_usd`. AI Gateway checks it before forwarding each request, and records cost after the request completes. Its enforcement is eventually consistent, so it can be briefly overshot by a burst. That's why it's the outer net and not the primary control. When it blocks, AI Gateway returns 429. The gateway Worker then stops trying further models and releases the reservation at $0.
- **Fallback routing on spend limits stays off,** because a cap must block, not reroute.
- **Every request carries `cf-aig-no-wholesale: true`,** so AI Gateway never falls back to Unified Billing (Cloudflare credits) when the stored key is missing. That fallback would bypass the OpenRouter key limit.
- **Every request carries `cf-aig-metadata: {app, task, plugin}`,** so analytics and optional per-task rules can split spend by task and plugin.

## Layer 3: the `cairn-gateway` Worker (primary, exact control)

- **No public URL.** `workers_dev = false`, no routes and no preview URLs. The only way in is the API Worker's `GATEWAY` Service Binding.
- **Sole spend credential.** It binds `CF_AIG_TOKEN` in production, or `OPENROUTER_API_KEY` for local dev only. It refuses to start if both are bound. The API Worker refuses to serve if it can see either.
- **Caps are compiled in.** `config/gateway.toml` is built into the Worker, so raising a cap means editing the file and running `make deploy-gateway`.
- **Ledger Durable Object** (SQLite, a single instance). It processes one request at a time with no awaits between the cap check and the insert, so reservations can't race past a cap.
- **Flow for each request** (`cairn-gateway-core::service::complete`):
  1. **Check the key**, at most hourly. A management or uncapped key disables the gateway. What happens if the check is *unreachable* depends on `key_check`.
  2. **Order the task's models.** The gateway may drop or reorder them. It never adds one.
  3. **Compute the worst case** from the allowlist price ceilings.
  4. **Reserve in the Ledger DO** against `min(task per_run, global per_request_max)` and the daily and monthly global, task and plugin caps.
  5. **Call** through AI Gateway with fixed routing: `sort: price`, `data_collection: deny`, `require_parameters: true`, and `max_price` set to the ceiling. Tasks may add only `zdr: true` or `allow_fallbacks: false`.
  6. **Settle** to `usage.cost`, or else the tokens priced at the ceiling, or else the worst case. A 4xx or 429 refusal is released at $0.
  7. **Check the breaker** for spend rate and rejection storms, and disable the gateway if either trips.
- **The kill switch ratchets:**
  - `POST /v1/admin/disable` (Discord `/ai off`) records `disabled_at`. There is no enable endpoint.
  - A disable is lifted only when a *deployed* `ENABLE_AFTER` var is later than `disabled_at`.

### `key_check`

The hourly `GET /key` goes through AI Gateway's OpenRouter passthrough, using the stored key. Cloudflare's docs only describe chat completions for OpenRouter, so whether `/key` is proxied has to be checked in M1.

| Setting | If `/key` is unreachable | When to use |
|---|---|---|
| `required` (default) | Completions are refused, which fails closed. The gateway is *not* disabled. | Whenever `/key` works through AI Gateway |
| `best-effort` | Completions proceed. The check is retried hourly, and reconcile reports `ok: null`. | Only if `/key` isn't proxied, and only after you've set both the AI Gateway spend limit and the OpenRouter key limit by hand |

A *positive* finding (a management key, or no limit) always disables the gateway, whichever setting you use.

## Tripwires

- **`builtin/reconcile`** (nightly) compares the ledger's UTC-day total with the Cairn key's `usage_daily`. Drift above `reconcile.tolerance_usd` disables the gateway. This is accurate only because the key is dedicated to Cairn.
- **Scheduler lint** rejects task graphs whose worst-case daily cost is above `global.per_day_usd`.
- **Repeated `upstream_limited` (429) responses** mean the AI Gateway rule or the OpenRouter limit is being hit before the ledger's own cap. Treat that as a sign the ledger and reality disagree, and investigate.

## `config/gateway.toml`

The committed file is the source of truth. Its model slugs and price ceilings are placeholders, so check them against OpenRouter's `/models` before you deploy. Validation fails the build if:
- the caps aren't ordered `per_request_max <= per_day <= per_month`
- the allowlist is empty
- any ceiling is zero or negative
