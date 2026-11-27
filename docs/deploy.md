# Deploy

Cairn runs on the VPS that also hosts Engine of Creation (EoC), with Docker Compose (ADR 0014), and uses that host's SurrealDB 4.x. Two containers:

- **`cairn`**: the API, the capture/task spool, the scheduler, live updates, and the web app. It's published on `127.0.0.1:8787` only.
- **`cairn-gateway`**: the only holder of a spend credential. It has no published port and is reachable only from `cairn`.

Your reverse proxy (Caddy or nginx) terminates TLS and forwards to `127.0.0.1:8787`. AI goes through Cloudflare AI Gateway with the OpenRouter key stored there (BYOK); that part doesn't care where Cairn runs.

## VPS: one-time setup

1. **SurrealDB (EoC's instance).**
   - Edit `deploy/surreal-init.surql`, replacing `CHANGE_ME` with a long random password. Then run it as root:
     ```sh
     surreal sql --endpoint http://127.0.0.1:8000 --user root --pass '…' < deploy/surreal-init.surql
     ```
     This creates namespace `cairn`, database `main`, and a **database-level** user `cairn` (role EDITOR). Cairn never touches EoC's namespaces, and this user can't log in to them.
   - Make sure the `cairn` container can reach SurrealDB:
     - **Surreal runs on the host** (systemd or a binary): it must listen on an address the Docker bridge can reach, such as `0.0.0.0:8000` behind a firewall, or the `docker0` address. Then `SURREAL_URL=http://host.docker.internal:8000`. A Surreal bound only to `127.0.0.1` is unreachable from containers.
     - **Surreal runs in EoC's compose project:** use `compose.eoc-network.yaml` (set EoC's network name in it) and `SURREAL_URL=http://<surreal service name>:8000`. Only `cairn` joins that network, never the gateway.
   - Optional, recommended: run the Store contract test against that server from a machine with Rust. It creates and then removes a throwaway database, so it needs root or a namespace-level user:
     ```sh
     CAIRN_SURREAL_URL=http://<host>:8000 CAIRN_SURREAL_USER=root CAIRN_SURREAL_PASS=… CAIRN_SURREAL_NS=cairn_test make test-live
     ```
2. **OpenRouter.**
   - Create a **dedicated** regular API key for Cairn, with a **credit limit and reset**. Turn auto top-up off.
   - Never create a management key for this.
3. **Cloudflare AI Gateway** (holds the OpenRouter key, BYOK). The Workers plan doesn't matter for this.
   - Create a gateway named `cairn`, turn on **Authenticated Gateway**, and create an auth token. That token is `CF_AIG_TOKEN`.
   - Under **Provider Keys**, add the OpenRouter key from step 2 with the alias `cairn`.
   - Under **Settings → Spend limits**, add a daily rule at about 1.2× `global.per_day_usd`, and leave fallback routing **off**.
   - Turn on **Require provider credentials**. The gateway also sends `cf-aig-no-wholesale: true`, so a missing stored key fails instead of billing Cloudflare credits.
4. **Env files** (both gitignored):
   ```sh
   cp deploy/cairn.env.example deploy/cairn.env
   cp deploy/gateway.env.example deploy/gateway.env
   ```
   - `deploy/cairn.env` gets `SURREAL_URL`, `SURREAL_PASS` (from step 1) and `CAIRN_TOKENS` (hash each token with `make hash`).
   - `deploy/gateway.env` gets `CF_AIG_TOKEN`, plus your account ID in `AIG_BASE_URL`.
   - Each container refuses to start if it can see the other one's secrets.
   - Run `chmod 600 deploy/*.env`.
5. **Start, then migrate:**
   ```sh
   make vps-up        # docker compose up -d --build
   make vps-migrate   # docker compose run --rm cairn migrate (idempotent)
   ```
6. **Reverse proxy.** Point a hostname at `127.0.0.1:8787`.
   - **Caddy:** see `deploy/Caddyfile.example`. TLS and WebSockets need no extra setup.
   - **nginx:** pass the WebSocket upgrade for `/hub`:
     ```nginx
     location / {
         proxy_pass http://127.0.0.1:8787;
         proxy_http_version 1.1;
         proxy_set_header Upgrade $http_upgrade;
         proxy_set_header Connection "upgrade";
         proxy_set_header Host $host;
         proxy_read_timeout 1h;
     }
     ```
   - Nothing sits in front of Cairn's own auth now: every route needs a scoped token, a webhook token or a Discord signature. Use long random tokens.
7. **Discord** (optional): set the interactions endpoint to `https://<host>/integrations/discord/interactions`, enable `[discord]` in config, and add `DISCORD_*` to `deploy/cairn.env`.

## VPS: every deploy

```sh
git pull
make vps-up          # rebuilds and recreates changed containers; the spool and ledger volumes persist
make vps-migrate     # apply new schema/*.surql
```

## VPS: raising a cap or re-enabling AI

Both need operator access to the host, on purpose:
- **To raise a cap:** edit `config/gateway.toml`, then rebuild the gateway: `docker compose up -d --build cairn-gateway`. Caps are compiled into the binary.
- **To re-enable after a disable:** set `ENABLE_AFTER` in `deploy/gateway.env` to the current time (RFC 3339), then `docker compose up -d --force-recreate cairn-gateway`.

## VPS: operations

- **Logs:** `docker compose logs -f cairn` or `docker compose logs -f cairn-gateway`. Set `RUST_LOG=debug` in the env file for more.
- **Spool:**
  - `docker compose exec cairn cairn spool status` shows `{pending, dead}`.
  - `pending` should be near zero. It grows while SurrealDB is down and drains on its own when it's back.
  - `dead` rows ran out of retries (10 for captures, 3 for task firings) and are kept. Fix the cause, then run `docker compose exec cairn cairn spool retry-dead`.
- **Backups:**
  - SurrealDB, Cairn's namespace only:
    ```sh
    surreal export --endpoint http://127.0.0.1:8000 --user root --pass … --ns cairn --db main cairn-$(date +%F).surql
    ```
  - Also the two volumes: `cairn_cairn-spool` (normally near-empty) and `cairn_cairn-ledger` (spend history).
- **SurrealDB upgrades** (shared with EoC): re-run `make test-live` against the upgraded server before relying on it.
- **Config without a rebuild:** mount a file and set `CAIRN_CONFIG` (see `deploy/cairn.env.example`). Otherwise `config/cairn.toml` is embedded at build time.

## Local development (native, no containers)

```sh
make db                       # local SurrealDB at :8000 (root/root, in memory)
make db-init                  # create ns `cairn` / db `main`: SurrealDB 3+/4 won't create them on first use
make test-live                # optional: the Store contract against that server
make web-build                # web/dist, served by cairn
make run-gateway              # terminal 1: cairn-gateway on :8788 (needs OPENROUTER_API_KEY or CF_AIG_TOKEN)
make run-server               # terminal 2: cairn on :8787 against the local SurrealDB, as root
make migrate TOKEN=<admin token> API=http://127.0.0.1:8787
```

`make run-server` reads `CAIRN_TOKENS` from your environment (the target's comment shows the format). Spool and ledger files go to `.local/`, which is gitignored.

## Legacy: Cloudflare Workers

ADR 0014 superseded this path. It still builds (`make build-workers`) and works as described below, until `workers/*` is removed.

### Plan and CPU limits

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

### One-time setup

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

### Every deploy

```sh
make deploy            # web build → gateway → api (gateway first: the api binds to it)
make migrate TOKEN=…   # apply new schema/*.surql (idempotent; tracked in _migration)
```

### Raising a cap or re-enabling AI

Both require a deploy, on purpose:
- **To raise a cap:** edit `config/gateway.toml`, then run `make deploy-gateway`.
- **To re-enable after a disable:** set `ENABLE_AFTER` in `workers/gateway/wrangler.toml` to the current time (RFC 3339), then run `make deploy-gateway`.

### Local development

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
