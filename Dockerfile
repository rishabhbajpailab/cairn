# Cairn on the VPS (ADR 0014). Two runtime images from one build:
#   docker compose build      (see compose.yaml; targets `cairn` and `gateway`)
# The gateway image contains only the gateway binary, and the cairn image doesn't contain it.

# ── web app ──────────────────────────────────────────────────────────────────
FROM node:22-bookworm-slim AS web
WORKDIR /src
RUN corepack enable
COPY web/package.json web/pnpm-lock.yaml web/
RUN cd web && pnpm install --frozen-lockfile
COPY plugins plugins
COPY web web
RUN cd web && pnpm build

# ── Rust binaries ────────────────────────────────────────────────────────────
FROM rust:1.97-bookworm AS build
WORKDIR /src
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p cairn-server -p cairn-gateway-server \
 && mkdir -p /out && cp target/release/cairn target/release/cairn-gateway /out/

# ── runtime base ─────────────────────────────────────────────────────────────
FROM debian:bookworm-slim AS base
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && useradd --system --uid 10001 --home /data cairn \
 && mkdir -p /data && chown cairn:cairn /data
USER cairn
VOLUME /data

# ── cairn: API, spool, scheduler, hub, web ───────────────────────────────────
FROM base AS cairn
COPY --from=build /out/cairn /usr/local/bin/cairn
COPY --from=web /src/web/dist /app/web
ENV CAIRN_BIND=0.0.0.0:8787 CAIRN_WEB_DIR=/app/web SPOOL_PATH=/data/spool.sqlite
EXPOSE 8787
ENTRYPOINT ["/usr/local/bin/cairn"]

# ── gateway: the only image that ever sees a spend credential ────────────────
FROM base AS gateway
COPY --from=build /out/cairn-gateway /usr/local/bin/cairn-gateway
ENV GATEWAY_BIND=0.0.0.0:8788 LEDGER_PATH=/data/ledger.sqlite
EXPOSE 8788
ENTRYPOINT ["/usr/local/bin/cairn-gateway"]
