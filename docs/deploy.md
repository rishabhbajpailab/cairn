# Deploy

Everything runs on Cloudflare plus Surreal Cloud. You need `wrangler` (`npm i -g wrangler`), a Rust toolchain with the `wasm32-unknown-unknown` target, and `pnpm`.

## Plan and CPU limits

**Use the Workers Paid plan.** The Free plan allows 10 ms of CPU time per HTTP request; Paid allows up to 5 minutes ([limits](https://developers.cloudflare.com/workers/platform/limits/)). Time spent waiting on the network (SurrealDB, the gateway, Discord) doesn't count, but everything Rust does in wasm does, including the Rust↔JavaScript conversions at the binding boundary.

The cost to watch is the **first request in a new isolate**, which builds the config and plugin registry (cached per isolate after that):

- Plugin JSON Schemas are **compiled at build time**. `cairn-plugins-embedded` turns each schema into Rust with `jsonschema::validator`, so loading the registry only parses the small manifests. An invalid schema fails the build rather than the first request.
- Measured with `make coldstart`, wasm under Node (V8, like workerd), 8 schemas:

  | | first load | warm reload |
  |---|---|---|
  | compiled at build time (what the Worker does) | 3–4.5 ms | ~0.2 ms |
  | compiled at runtime (`FsSource`, the old path) | 30–45 ms | 8–11 ms |

  Runtime compilation alone was over three times the Free plan's budget, mostly because every schema was checked against the draft 2020-12 meta-schema, and on wasm that meta-validator isn't cached.
- The API Worker logs a warning if any plugin schema ever ends up compiled at runtime (`Registry::runtime_compiled`).
- Even precompiled, a cold request still spends a few milliseconds on wasm instantiation, the registry, TOML config, routing and JSON. Discord signature checks (ed25519) add more. On Free that leaves little headroom. On Paid it's irrelevant.
- To check in production, look at **CPU time** per invocation in Workers Logs, or run `wrangler tail`.

## One-time setup

1. **Surreal Cloud.**
   - Create an instance (Earth, or a dedicated Cloud node; see ADR 0001).
   - Create namespace `cairn` and database `main` (SurrealDB doesn't create them on first use), then a database-level user for them.
   - Optionally run the Store contract against it with a throwaway database: `make test-live SURREAL=https://… ` with `CAIRN_SURREAL_USER` / `CAIRN_SURREAL_PASS` set to a user allowed to create databases in `CAIRN_SURREAL_NS`. It removes its database afterwards.
   - Put the HTTPS endpoint in `config/cairn.toml` → `[surreal].url`.
2. **OpenRouter.**
   - Create a **dedicated** regular API key for Cairn, with a **credit limit and reset**. Don't reuse a key other apps spend from: the nightly reconcile compares Cairn's ledger against this key's usage.
   - Turn auto top-up off.
   - Optionally set an account-wide provider allowlist.
   - Never create a management key for this.
3. **Cloudflare AI Gateway** (holds the OpenRouter key, BYOK):
   - Create a gateway named `cairn`, and turn on **Authenticated Gateway**.
   - Create an auth token for it. This becomes `CF_AIG_TOKEN`.
   - Under **Provider Keys**, add the OpenRouter key from step 2 with the alias `cairn`. It's stored in Secrets Store, and no Worker ever sees it.
   - Under **Settings → Spend limits**, add one rule for all traffic, set slightly above `global.per_day_usd` (say 1.2×) on a daily window. This is an independent outer cap in case the ledger is wrong. Optionally add a rule filtered on metadata `app = cairn`, if other apps share the gateway.
   - Leave spend-limit fallback routing **off**: a limit should block, not reroute.
   - Turn on **Require provider credentials** (no Unified Billing fallback). The gateway also sends `cf-aig-no-wholesale: true` on every request, so a missing stored key fails with error 2044 instead of billing your Cloudflare credits.
   - Put your account ID into `AIG_BASE_URL` in `workers/gateway/wrangler.toml`.
4. **Queues:**
   ```sh
   wrangler queues create cairn-captures
   wrangler queues create cairn-captures-dlq
   wrangler queues create cairn-tasks
   wrangler queues create cairn-tasks-dlq
   ```
5. **Secrets.** Each Worker has its own, and they are never shared:
   ```sh
   cd workers/gateway && wrangler secret put CF_AIG_TOKEN   # the AI Gateway token, NOT the OpenRouter key
   cd workers/api
   wrangler secret put CAIRN_TOKENS        # "phone:capture:<sha256>;ui:write:<sha256>;me:admin:<sha256>"
   wrangler secret put SURREAL_USER
   wrangler secret put SURREAL_PASS
   wrangler secret put DISCORD_PUBLIC_KEY  # if [discord].enabled
   wrangler secret put DISCORD_BOT_TOKEN
   ```
   Get each token hash with `make hash`.
6. **Cloudflare Access.**
   - Protect the web UI hostname with an Access application.
   - Bypass these machine paths, which carry their own auth:
     - `/api/v1/capture`
     - `/ingest/webhook/*`
     - `/integrations/discord/interactions`
     - `/healthz`
7. **Discord** (optional).
   - Set the interactions endpoint URL to `https://<api-host>/integrations/discord/interactions`.
   - Register the slash commands `/c`, `/todo`, `/remind`, `/find` and `/ai`, plus the message command "Save to Cairn".

## Every deploy

```sh
make deploy            # web build → gateway → api (gateway first: the api binds to it)
make migrate TOKEN=…   # apply new schema/*.surql (idempotent; tracked in _migration)
```

## Raising a cap or re-enabling AI

Both require a deploy, on purpose:
- **To raise a cap:** edit `config/gateway.toml`, then run `make deploy-gateway`.
- **To re-enable after a disable:** set `ENABLE_AFTER` in `workers/gateway/wrangler.toml` to the current time (RFC 3339), then run `make deploy-gateway`.

## Local development

```sh
make db                       # local SurrealDB in podman/docker at :8000 (root/root)
make db-init                  # create ns `cairn` / db `main`: SurrealDB 3+/4 won't create them on first use
make test-live                # optional: the Store contract against that server (tests/live_surreal.rs)
cp workers/api/.dev.vars.example workers/api/.dev.vars
cp workers/gateway/.dev.vars.example workers/gateway/.dev.vars
make dev-gateway              # terminal 1
make dev-api                  # terminal 2  (wrangler dev wires the Service Binding locally)
make web                      # terminal 3  (Vite, proxies /api and /hub to :8787)
make migrate TOKEN=<admin token> API=http://127.0.0.1:8787
```
