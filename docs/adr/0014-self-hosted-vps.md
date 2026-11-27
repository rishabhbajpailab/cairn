# 0014: Run on the VPS (Docker Compose), not Cloudflare Workers

- **Status:** accepted (supersedes 0008, 0009 and 0010; amends 0013)
- **Date:** 2026-11-27

## Context

There's spare capacity on the VPS that already runs Engine of Creation (EoC), and that host already runs SurrealDB 4.x locally. A persistent process removes the constraints that shaped the Workers design: per-request CPU limits, cold isolates, no filesystem, Queues and Durable Objects as the only durable primitives, and a database across the internet. Cairn is single-user, so one host is enough.

The libraries were written against ports (`cairn_app::ports`, `HttpTransport`, `LedgerPort`, `LedgerSql`), so only the glue changes.

## Decision

Two native binaries, run with Docker Compose next to EoC, behind a reverse proxy on the host:

| Workers piece | VPS replacement |
|---|---|
| API Worker (`workers/api`) | `cairn` (`hosts/server`): axum on tokio, one persistent process |
| CAPTURES / TASKS Queues + DLQs | A durable **SQLite spool** on a volume (`spool.rs`): written and fsynced before the 202, consumed by a background loop with retries, backoff and a dead-letter state |
| Scheduler DO alarm + hourly cron | A **tokio timer** that sleeps until the next due reminder or cron task, is woken by `rearm`, and has an hourly backstop |
| Hub DO | An in-process broadcast channel feeding `/hub` WebSockets |
| Static assets | `web/dist` served by `cairn` (SPA fallback) |
| Gateway Worker + Service Binding | `cairn-gateway` (`hosts/gateway`), its own container on a private network shared only with `cairn`, with no published port |
| Ledger DO (SQLite) | SQLite file on the gateway's own volume, behind a mutex |
| Surreal Cloud | EoC's `index` container (SurrealDB 4.0.1). Cairn gets its own namespace (`cairn`) and a database-level user, and never touches EoC's `eoc` namespace |

Ingress is a reverse proxy with TLS: EoC's door (see the amendment below), or the host's own proxy to `127.0.0.1:8787` when running standalone. Cloudflare Access no longer sits in front, so every route relies on Cairn's own token auth (it already did; Access was defence in depth).

AI still goes through Cloudflare AI Gateway with the OpenRouter key stored there (BYOK, ADR 0012). That's plain HTTPS and doesn't depend on where Cairn runs.

### How the invariants hold

1. **Captures are never lost after auth.** The capture is committed to the spool (`synchronous = FULL`, WAL) before the 202. The spool is on a volume, so it survives restarts, and it keeps retrying while SurrealDB is down. The consumer is still idempotent.
5. **Only the gateway holds a spend credential.** `CF_AIG_TOKEN` is only in the gateway's env file. `cairn` refuses to start if it can see `CF_AIG_TOKEN` or `OPENROUTER_API_KEY`, and `cairn-gateway` refuses if it can see the Surreal credentials. CI's upstream grep covers `hosts/` too.
6. **Code can only tighten.** Caps are still compiled into the gateway binary from `config/gateway.toml`. `ENABLE_AFTER` is in the gateway's env file, so lifting a disable means an operator editing that file and recreating the container. The `cairn` container can't reach the gateway's env or volume. There's still no enable endpoint.
7. **Durable Objects handled one request at a time.** The SQLite connection behind a mutex gives the same guarantee, and the `Ledger` calls are synchronous, so nothing can interleave between check and write.
9. **Nothing polls.** The scheduler sleeps until the next event (or the hourly backstop) and is woken early by `rearm`. The spool consumer sleeps until it's notified, or until the next retry is due.

## Amendment, 2026-11-27: sharing EoC's index and door

The author decided that Cairn uses EoC's `index` SurrealDB server itself, sharing the load rather than running a second database, and goes through EoC's door, which owns ports 80 and 443. EoC's index is disposable *by design* (EoC must survive losing it), but it's not treated as disposable in operation.

- `compose.eoc.yaml` (the standard deployment, `make vps-up`) joins `cairn` to two internal networks that EoC's compose file creates with fixed names: `eoc-cairn-index` (cairn ↔ index) and `eoc-cairn-door` (door ↔ cairn). It also drops the published port.
- `cairn` never joins EoC's `inside` network, where EoC's unauthenticated `book` lives, and the gateway joins neither network.
- EoC's door serves `CAIRN_ADDRESS` from `sites/cairn.caddy`. Cairn isn't behind EoC's GitHub sign-in, because its machine paths need tokens instead (open question, recorded in EoC's DECISIONS.md).
- Cairn's namespace is backed up separately (`make vps-backup`, which runs `surreal export` as Cairn's own database user).
- Before EoC raises the index version, Cairn's contract is run against that version locally (`make db SURREAL_VERSION=…`).
- Memory limits on both containers (256 MB and 128 MB; measured at about 15 MB and 6 MB) keep Cairn from crowding EoC on a small server.

## Consequences

- The VPS is a single point of failure. While it's down, captures are refused rather than queued at an edge. A spool-at-the-edge Worker was considered and rejected for now (more moving parts for a single-user app).
- No per-request CPU limit, so ADR 0013's precompiled schemas are now a startup-time and latency nicety rather than a requirement. They stay.
- Cairn shares a host and a SurrealDB process with EoC: resource contention, and EoC's Surreal upgrades affect Cairn. Run `make test-live` against that instance after Surreal upgrades.
- Backups are our job now: the SurrealDB `cairn` namespace (`surreal export`), the spool volume (normally near-empty), and the gateway ledger volume.
- `workers/*` stays in the repo until the VPS deploy has run for a while, then gets deleted. The libraries keep compiling for wasm32 in the meantime, which costs nothing.
