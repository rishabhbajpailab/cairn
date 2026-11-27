# Deploy

Cairn runs on the VPS that also hosts Engine of Creation (EoC), with Docker Compose (ADR 0014). It shares EoC's SurrealDB (EoC's `index` container, 4.0.1) and EoC's door (Caddy, the only thing with ports 80 and 443). Two containers:

- **`cairn`**: the API, the capture/task spool, the scheduler, live updates, and the web app. No published port: EoC's door reaches it over a network of its own.
- **`cairn-gateway`**: the only holder of a spend credential. It has no published port and is reachable only from `cairn`.

```
 Internet ─▶ door (EoC, Caddy) ─ eoc-cairn-door ─▶ cairn ─ eoc-cairn-index ─▶ index (EoC's SurrealDB)
                    │                                │
                 inside ─▶ sign-in ─▶ book         gateway (internal) ─▶ cairn-gateway ─▶ AI Gateway
```

`cairn` is never on EoC's `inside` network, so it can't reach EoC's book, which has no sign-in of its own. EoC's side of this is written up in its `docs/SETUP.md` ("Sharing the server with Cairn") and `docs/DECISIONS.md` ("Cairn shares the rented server").

AI goes through Cloudflare AI Gateway with the OpenRouter key stored there (BYOK); that part doesn't care where Cairn runs.

Running Cairn **standalone** (its own SurrealDB, its own proxy) is `compose.yaml` alone: `make vps-up COMPOSE="docker compose"`, `SURREAL_URL` in `deploy/cairn.env`, and a reverse proxy to `127.0.0.1:8787` (`deploy/Caddyfile.example`).

## VPS: one-time setup

1. **EoC first.** EoC's compose project must be up, with its changes for Cairn (the `eoc-cairn-index` and `eoc-cairn-door` networks, and the door's `sites/` import). Its `docker compose up -d` creates the two networks.
2. **Cairn's namespace and user in EoC's SurrealDB.** From EoC's folder on the server (`~/engine-of-creation`, its copied `deploy` folder; Cairn's clone sits beside it at `~/cairn`), make up Cairn's password and run the init script with EoC's root password (read from EoC's `.env`):
   ```sh
   CAIRN_PASS="$(openssl rand -hex 24)"; echo "SURREAL_PASS for deploy/cairn.env: $CAIRN_PASS"
   sed "s/CHANGE_ME/$CAIRN_PASS/" ../cairn/deploy/surreal-init.surql | \
     docker run --rm -i --network eoc-cairn-index surrealdb/surrealdb:v4.0.1 sql \
     --endpoint http://index:8000 --user root --pass "$(sed -n 's/^INDEX_PASSWORD=//p' .env)" --hide-welcome
   ```
   - It runs in a **throwaway container** on `eoc-cairn-index`, not inside `index`, because `surreal sql` saves everything it's given to a `history.txt` in its working directory, Cairn's password included. The throwaway container takes that file with it. The password is substituted on the way in, so it's never written into the checkout.
   - It creates namespace `cairn`, database `main`, and a **database-level** user `cairn` (role EDITOR). Cairn never touches EoC's `eoc` namespace, and this user can't log in to it (tried). EoC's rebuild only rewrites rows in `eoc` and never removes a namespace.
   - Use the same image tag as EoC's `index`.
3. **OpenRouter.**
   - Create a **dedicated** regular API key for Cairn, with a **credit limit and reset**. Turn auto top-up off.
   - Never create a management key for this.
4. **Cloudflare AI Gateway** (holds the OpenRouter key, BYOK). The Workers plan doesn't matter for this.
   - Create a gateway named `cairn`, turn on **Authenticated Gateway**, and create an auth token. That token is `CF_AIG_TOKEN`.
   - Under **Provider Keys**, add the OpenRouter key from step 2 with the alias `cairn`.
   - Under **Settings → Spend limits**, add a daily rule at about 1.2× `global.per_day_usd`, and leave fallback routing **off**.
   - Turn on **Require provider credentials**. The gateway also sends `cf-aig-no-wholesale: true`, so a missing stored key fails instead of billing Cloudflare credits.
5. **Env files** (both gitignored):
   ```sh
   cp deploy/cairn.env.example deploy/cairn.env
   cp deploy/gateway.env.example deploy/gateway.env
   ```
   - `deploy/cairn.env` gets `SURREAL_PASS` (from step 2) and `CAIRN_TOKENS` (hash each token with `make hash`). `compose.eoc.yaml` sets `SURREAL_URL=http://index:8000`.
   - `deploy/gateway.env` gets `CF_AIG_TOKEN`, plus your account ID in `AIG_BASE_URL`.
   - Each container refuses to start if it can see the other one's secrets.
   - Run `chmod 600 deploy/*.env`.
6. **Start, then migrate** (from Cairn's folder):
   ```sh
   make vps-up        # docker compose -f compose.yaml -f compose.eoc.yaml up -d --build
   make vps-migrate   # `cairn migrate` in a one-off container (idempotent)
   ```
7. **A name, through EoC's door.** Point a DNS name at the server, like the book's. In EoC's folder:
   - set `CAIRN_ADDRESS` in `.env`;
   - copy `sites/cairn.caddy.example` to `sites/cairn.caddy`;
   - recreate the door with `docker compose up -d --force-recreate door`.

   Caddy gets the certificate and passes WebSockets (`/hub`) through without extra setup.
   - **Cairn is not behind EoC's GitHub sign-in.** Phones, scripts and Discord post with Cairn's own tokens, which a sign-in page would block. Every route needs a scoped token, a webhook token or a Discord signature, so use long random tokens. Putting the web pages behind the sign-in, with the machine paths let through, is an open question in EoC's DECISIONS.md.
8. **Discord** (optional): set the interactions endpoint to `https://<host>/integrations/discord/interactions`, enable `[discord]` in config, and add `DISCORD_*` to `deploy/cairn.env`.

## VPS: every deploy

```sh
git pull
make vps-up          # rebuilds and recreates changed containers; the spool and ledger volumes persist
make vps-migrate     # apply new schema/*.surql
```

## VPS: raising a cap or re-enabling AI

Both need operator access to the host, on purpose:
In the commands below, `$COMPOSE` means `docker compose -f compose.yaml -f compose.eoc.yaml`, which is the Makefile's default.

- **To raise a cap:** edit `config/gateway.toml`, then rebuild the gateway: `$COMPOSE up -d --build cairn-gateway`. Caps are compiled into the binary.
- **To re-enable after a disable:** set `ENABLE_AFTER` in `deploy/gateway.env` to the current time (RFC 3339), then `$COMPOSE up -d --force-recreate cairn-gateway`.

## VPS: operations

- **Logs:** `make vps-logs`, or `$COMPOSE logs -f cairn`. Set `RUST_LOG=debug` in the env file for more.
- **Spool:**
  - `$COMPOSE exec cairn cairn spool status` shows `{pending, dead}`.
  - `pending` should be near zero. It grows while SurrealDB is down and drains on its own when it's back.
  - `dead` rows ran out of retries (10 for captures, 3 for task firings) and are kept. Fix the cause, then run `$COMPOSE exec cairn cairn spool retry-dead`.
- **Backups:**
  - **Cairn's namespace** is the one thing here that can't be rebuilt. EoC's index is disposable by design (EoC refills its own part from the book), but Cairn's part isn't, so back it up. Use `make vps-backup`, set `EOC=<EoC's folder>` if it isn't `../engine-of-creation`. It runs `surreal export` inside EoC's `index` container as Cairn's own user, so no root password is involved, and writes `backups/cairn-<date>.surql` (gitignored). Schedule it with cron, and copy the files off the server.
  - Restoring into an empty namespace: run `surreal-init.surql` again, then `surreal import` the file as the `cairn` user.
  - Also the two volumes: `cairn_cairn-spool` (normally near-empty) and `cairn_cairn-ledger` (spend history).
- **SurrealDB upgrades** (shared with EoC, which pins `index` to its `surrealdb` crate). Before EoC raises the version, run Cairn's contract against the new one locally: `make db SURREAL_VERSION=v4.x.y && make db-init && make test-live`. EoC's index publishes no port, so the test can't run against it directly.
- **Config without a rebuild:** mount a file and set `CAIRN_CONFIG` (see `deploy/cairn.env.example`). Otherwise `config/cairn.toml` is embedded at build time.

## Local development (native, no containers)

```sh
make db                       # local SurrealDB at :8000 (root/root, in memory; SURREAL_VERSION as EoC's)
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
