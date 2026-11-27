.PHONY: vps-up vps-migrate vps-logs vps-backup run-server run-gateway
.PHONY: check fmt fmt-check lint test test-live build-workers check-wasm coldstart db-init web web-build types db dev-api dev-gateway deploy deploy-gateway deploy-api migrate hash

API ?= https://cairn.example.dev

check: fmt-check lint test             ## run before every commit

fmt:
	cargo fmt --all

fmt-check:
	cargo fmt --all -- --check

lint:
	cargo clippy --workspace --all-targets -- -D warnings

test:                                  ## native: no Cloudflare or database needed
	cargo test --workspace

SURREAL ?= http://127.0.0.1:8000
# Keep equal to the `index` image in engine-of-creation/deploy/compose.yaml. To check a newer
# version before EoC raises it: make db SURREAL_VERSION=v4.x.y && make db-init && make test-live
SURREAL_VERSION ?= v4.0.1
test-live:                             ## SurrealHttpStore against a real SurrealDB (default: local `make db`)
	CAIRN_SURREAL_URL=$(SURREAL) cargo test -p cairn-store --test live_surreal -- --nocapture

check-wasm:                            ## type-check the Workers for their real target
	cargo check -p cairn-api-worker -p cairn-gateway-worker --target wasm32-unknown-unknown

build-workers:
	cd workers/gateway && worker-build --release
	cd workers/api && worker-build --release

coldstart:                             ## CPU cost of loading plugins in a fresh isolate (wasm under Node)
	cargo build -q --release --target wasm32-wasip1 -p cairn-plugins-embedded --example coldstart
	@for m in embedded runtime; do PLUGINS=$(CURDIR)/plugins node --no-warnings scripts/wasi-run.mjs \
	  target/wasm32-wasip1/release/examples/coldstart.wasm $$m /plugins; done

types:                                 ## TS types from plugins/types/*/schema.json
	cd web && pnpm types

web:
	cd web && pnpm dev

web-build:
	cd web && pnpm install && pnpm build

db:                                    ## local SurrealDB for `wrangler dev` (root/root, in-memory)
	podman run --rm -d --name cairn-surreal -p 127.0.0.1:8000:8000 \
	  docker.io/surrealdb/surrealdb:$(SURREAL_VERSION) start --user root --pass root memory

db-init:                               ## create namespace `cairn` / database `main` (SurrealDB doesn't do it implicitly)
	curl -fsS -X POST "$(SURREAL)/sql" -u root:root -H 'accept: application/json' \
	  -d 'DEFINE NAMESPACE IF NOT EXISTS cairn; USE NS cairn; DEFINE DATABASE IF NOT EXISTS main;' >/dev/null && echo ok

# ── VPS (ADR 0014, docs/deploy.md) ────────────────────────────────────────────
# Standard VPS deployment: sharing EoC's SurrealDB and door (compose.eoc.yaml).
# Standalone: make vps-up COMPOSE="docker compose"
COMPOSE ?= docker compose -f compose.yaml -f compose.eoc.yaml
# EoC's folder on the server (its copied deploy/ folder, beside this clone), for vps-backup.
EOC ?= ../engine-of-creation

vps-up:                                ## build + (re)start both containers
	$(COMPOSE) up -d --build

vps-migrate:                           ## apply schema/*.surql (idempotent)
	$(COMPOSE) run --rm cairn migrate

vps-logs:
	$(COMPOSE) logs -f --tail=100

vps-backup:                            ## export Cairn's namespace from EoC's index, as Cairn's own user
	@mkdir -p backups
	@SURREAL_USER="$$(sed -n 's/^SURREAL_USER=//p' deploy/cairn.env)"; \
	  SURREAL_PASS="$$(sed -n 's/^SURREAL_PASS=//p' deploy/cairn.env)"; \
	  docker compose --project-directory "$(EOC)" exec -T index /surreal export \
	  --endpoint http://localhost:8000 --user "$$SURREAL_USER" --pass "$$SURREAL_PASS" --auth-level database \
	  --namespace cairn --database main - > backups/cairn-$$(date +%F-%H%M).surql
	@ls -l backups | tail -1

# Native, no containers, against `make db` (root/root). CAIRN_TOKENS comes from your shell,
# e.g. CAIRN_TOKENS="me:admin:$$(printf %s devtoken | sha256sum | cut -d' ' -f1)".
run-gateway:
	@mkdir -p .local
	LEDGER_PATH=.local/ledger.sqlite GATEWAY_BIND=127.0.0.1:8788 cargo run -p cairn-gateway-server

run-server:
	@mkdir -p .local
	SURREAL_URL=$(SURREAL) SURREAL_USER=root SURREAL_PASS=root SURREAL_AUTH_LEVEL=root \
	  SPOOL_PATH=.local/spool.sqlite CAIRN_WEB_DIR=web/dist CAIRN_BIND=127.0.0.1:8787 \
	  GATEWAY_URL=http://127.0.0.1:8788 cargo run -p cairn-server

# ── Legacy: Cloudflare Workers ────────────────────────────────────────────────
dev-gateway:
	cd workers/gateway && wrangler dev --port 8788

dev-api:
	cd workers/api && wrangler dev --port 8787

deploy: web-build deploy-gateway deploy-api   ## gateway first: the api binds to it

deploy-gateway:
	cd workers/gateway && wrangler deploy

deploy-api:
	cd workers/api && wrangler deploy

migrate:                               ## apply schema/*.surql: make migrate TOKEN=<admin> [API=…]
	@test -n "$(TOKEN)" || (echo "usage: make migrate TOKEN=<admin token> [API=$(API)]"; exit 1)
	curl -fsS -X POST "$(API)/api/v1/admin/migrate" -H "authorization: Bearer $(TOKEN)"; echo

hash:                                  ## sha256 for CAIRN_TOKENS
	@read -p "token: " t; printf '%s' "$$t" | sha256sum | cut -d' ' -f1
