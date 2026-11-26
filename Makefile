.PHONY: check fmt fmt-check lint test build-workers check-wasm coldstart web web-build types db dev-api dev-gateway deploy deploy-gateway deploy-api migrate hash

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
	  docker.io/surrealdb/surrealdb:v3 start --user root --pass root memory

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
