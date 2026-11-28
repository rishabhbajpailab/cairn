# Deploy

Cairn runs on the VPS that also hosts Engine of Creation (EoC), with Docker Compose (ADR 0014). It shares EoC's SurrealDB (EoC's `index` container, 4.0.1) and EoC's door (Caddy, the only thing with ports 80 and 443). Three containers:

- **`cairn-sign-in`**: oauth2-proxy, with Cairn's **own** GitHub OAuth App (ADR 0015). It's the only way in. Pages need the GitHub sign-in. The machine routes (capture, webhooks, Discord, health; `deploy/sign-in.cfg`) are let through to Cairn's own token checks.
- **`cairn`**: the API, the capture/task spool, the scheduler, live updates, and the web app. No published port, and not on any network the door is on: only `cairn-sign-in` reaches it.
- **`cairn-gateway`**: the only holder of a spend credential. It has no published port and is reachable only from `cairn`.

```
 Internet ─▶ door (EoC) ─ eoc-cairn-door ─▶ cairn-sign-in ─▶ cairn ─ eoc-cairn-index ─▶ index (EoC's SurrealDB)
                 │                                             │
              inside ─▶ sign-in ─▶ book                      gateway (internal) ─▶ cairn-gateway ─▶ AI Gateway
```

Nothing of Cairn's is on EoC's `inside` network, so it can't reach EoC's book, which has no sign-in of its own. EoC's side of this is written up in its `docs/SETUP.md` ("Sharing the server with Cairn") and `docs/DECISIONS.md` ("Cairn shares the rented server").

AI goes through Cloudflare AI Gateway with the OpenRouter key stored there (BYOK); that part doesn't care where Cairn runs.

Running Cairn **standalone** (its own SurrealDB, its own proxy) is `compose.yaml` alone: `make vps-up COMPOSE="docker compose"`, `SURREAL_URL` in `deploy/cairn.env`, and a reverse proxy to `127.0.0.1:8787`, which is the sign-in (`deploy/Caddyfile.example`).

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
   - Under **Provider Keys**, add the OpenRouter key from step 3 with the alias `cairn`.
   - Under **Settings → Spend limits**, add a daily rule at about 1.2× `global.per_day_usd`, and leave fallback routing **off**.
   - Turn on **Require provider credentials**. The gateway also sends `cf-aig-no-wholesale: true`, so a missing stored key fails instead of billing Cloudflare credits.
5. **Cairn's own GitHub OAuth App**, separate from EoC's. On GitHub: **Settings → Developer settings → OAuth Apps → New OAuth App**.
   - **Homepage URL:** `https://<CAIRN_ADDRESS>`
   - **Authorization callback URL:** `https://<CAIRN_ADDRESS>/oauth2/callback`
   - Then **Generate a new client secret**. It asks GitHub who you are, and for nothing about your repositories.
6. **Env files and secrets** (all gitignored):
   ```sh
   cp deploy/cairn.env.example deploy/cairn.env
   cp deploy/gateway.env.example deploy/gateway.env
   cp deploy/sign-in.env.example deploy/sign-in.env
   mkdir -m 700 deploy/secrets
   printf %s '<the client secret>' > deploy/secrets/github_client_secret   # no trailing newline
   openssl rand 32 > deploy/secrets/cookie_secret
   chmod 644 deploy/secrets/*      # oauth2-proxy runs as a non-root user; the folder stays 700
   chmod 600 deploy/*.env
   ```
   - `deploy/cairn.env` gets `SURREAL_PASS` (from step 2) and `CAIRN_TOKENS` (hash each token with `make hash`). `compose.eoc.yaml` sets `SURREAL_URL=http://index:8000`.
   - `deploy/gateway.env` gets `CF_AIG_TOKEN`, plus your account ID in `AIG_BASE_URL`.
   - `deploy/sign-in.env` gets the OAuth App's client ID, the callback URL, and your GitHub user name: the one account that may sign in.
   - Each container refuses to start if it can see the other one's secrets.
7. **Start, then migrate** (from Cairn's folder):
   ```sh
   make vps-up        # docker compose -f compose.yaml -f compose.eoc.yaml up -d --build
   make vps-migrate   # `cairn migrate` in a one-off container (idempotent)
   ```
8. **A name, through EoC's door.** Point a DNS name at the server, like the book's. In EoC's folder:
   - set `CAIRN_ADDRESS` in `.env`;
   - copy `sites/cairn.caddy.example` to `sites/cairn.caddy`. It proxies to `cairn-sign-in:4180`, never to `cairn`;
   - recreate the door with `docker compose up -d --force-recreate door`.

   Caddy gets the certificate and passes WebSockets (`/hub`) through without extra setup. Open `https://<CAIRN_ADDRESS>`: GitHub asks you to sign in, then the app asks for your `ui` token.
   - **What needs what:** pages and the app's API need the GitHub sign-in **and** a Cairn token. The machine routes need only their own credential: `POST /api/v1/capture` (a capture token, as used by the iOS Shortcut), `POST /ingest/webhook/<source>` (a webhook token), `POST /integrations/discord/interactions` (Discord's signature), and `GET /healthz`.
   - Adding a machine route means adding it to `cairn_app::api::MACHINE_ROUTES` and `deploy/sign-in.cfg`. A test fails if they differ.
9. **Discord** (optional): set the interactions endpoint to `https://<host>/integrations/discord/interactions`, enable `[discord]` in config, and add `DISCORD_*` to `deploy/cairn.env`.

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
- **Backups, weekly, to S3 or R2** (ADR 0015). Cairn's namespace is the one thing here that can't be rebuilt: EoC's index is disposable by design (EoC refills its own part from the book), but Cairn's part isn't.
  1. Make a **private bucket** of its own, with a key scoped to it: in R2, an "Object Read & Write" token for that bucket; in S3, an IAM user allowed only `s3:PutObject` on `<bucket>/cairn/*`. Add a lifecycle rule that deletes objects after about 180 days.
  2. `cp deploy/backup.env.example deploy/backup.env`, fill it in, and `chmod 600` it.
  3. Try it once with `make vps-backup-upload`. It runs `deploy/backup.sh`, which:
     - exports Cairn's namespace inside EoC's `index` container, as Cairn's own user (no root password);
     - checks that the export defines Cairn's tables, then compresses it to `backups/cairn-<UTC time>.surql.gz`;
     - uploads it with rclone, in a container, so nothing is installed on the host;
     - deletes local copies older than 35 days.

     It fails, and uploads nothing, if the export fails or looks empty. `EOC=<EoC's folder>` if it isn't `../engine-of-creation`.
  4. Add it to the host's crontab (`crontab -e`), for example Sundays at 03:17:
     ```
     17 3 * * 0  cd ~/cairn && sh deploy/backup.sh >> backups/backup.log 2>&1
     ```
     Check `backups/backup.log` now and then. The cron line gives no other sign of a failure.
  - `make vps-backup` makes a local copy only, for example before an upgrade.
  - Not in the backup: the spool volume (normally empty) and the ledger volume (spend history). The VPS provider's server snapshots cover those.
- **Restoring Cairn's namespace** (it must be empty: a fresh server, or after `REMOVE DATABASE main` in namespace `cairn` as root):
  1. Run step 2 of the one-time setup again. It recreates the namespace, the database and the `cairn` user; backups don't contain the user, because SurrealDB redacts password hashes in exports.
  2. From Cairn's folder, as Cairn's own user, in a throwaway container. `surreal sql` writes everything it's given to `history.txt`, here the whole backup, and Cairn's user may not use `surreal import`, which needs OWNER:
     ```sh
     zcat backups/cairn-<time>.surql.gz | docker run --rm -i --network eoc-cairn-index surrealdb/surrealdb:v4.0.1 sql \
       --endpoint http://index:8000 --user cairn --pass "$(sed -n 's/^SURREAL_PASS=//p' deploy/cairn.env)" \
       --auth-level database --namespace cairn --database main --hide-welcome
     ```
     To fetch a file from the bucket first: `docker run --rm --env-file deploy/backup.env -v "$PWD/backups:/backups" rclone/rclone:1.68.2 copy backup:<bucket>/cairn/<file> /backups/`.
  3. `make vps-up`. Tried end to end: items, the ops log, captures, the migration record and the ops-log guard all come back.
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
