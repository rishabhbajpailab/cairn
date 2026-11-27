# Cairn

A self-hosted second brain. Capture anything from your phone, scripts, webhooks or Discord into one typed graph. Arrange it on a visual canvas with buckets, tags and typed links. Let deterministic, hard-capped AI tasks summarize it, sort it and remind you about it.

**Runs on:** a VPS with Docker Compose (two small Rust containers), SurrealDB 4.x, and OpenRouter through Cloudflare AI Gateway. It started on Cloudflare Workers; that path still builds (`workers/*`) but is legacy.

- **Everything is an item.** To-dos, hold-that-thoughts, reminders, buckets, task definitions and their runs are all items. Plugins define each type's schema *and* its interactive renderers.
- **Captures are never lost.** They're committed to a local spool on disk as soon as they arrive, and written to the database afterwards, even if the database is down for a while.
- **AI is boxed in.**
  - You define every task: its trigger, inputs, model list and budget.
  - A separate gateway container with no published port is the only path to models. It reaches OpenRouter through Cloudflare AI Gateway, which keeps the OpenRouter key in its own keystore, so nothing on the VPS ever holds the key.
  - Its spend caps are compiled into the build, so changing them takes a rebuild on the host.

## Quick start

```sh
make test                      # native tests, no database needed
make db && make db-init        # local SurrealDB 4.x, namespace cairn / database main
make web-build
make run-gateway               # terminal 1 (OPENROUTER_API_KEY=… for local AI, optional)
CAIRN_TOKENS="me:admin:$(printf %s devtoken | sha256sum | cut -d' ' -f1)" make run-server   # terminal 2
make migrate TOKEN=devtoken API=http://127.0.0.1:8787
```

To deploy on the VPS, see [docs/deploy.md](docs/deploy.md).

## Docs

| | |
|---|---|
| How it fits together | [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) |
| What's next | [docs/ROADMAP.md](docs/ROADMAP.md) |
| Deploy and local dev | [docs/deploy.md](docs/deploy.md) |
| Item types and AI plugins | [docs/plugins.md](docs/plugins.md) |
| Task definitions | [docs/tasks.md](docs/tasks.md) |
| Spend control (3 layers, AI Gateway BYOK) | [docs/spend-control.md](docs/spend-control.md) |
| Archive and GC | [docs/lifecycle-gc.md](docs/lifecycle-gc.md) |
| REST / webhooks / Discord | [docs/ingest.md](docs/ingest.md) |
| Decisions | [docs/adr/](docs/adr/) |
| Working with Claude Code | [CLAUDE.md](CLAUDE.md) |

## Status

**M0 skeleton.** All library logic is unit-tested natively (`make check`), and the Worker glue type-checks.

**Not run yet:**
- a WebAssembly build of the Workers
- anything against a live SurrealDB

Those are the first items in M1.
