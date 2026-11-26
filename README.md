# Cairn

A cloud-native second brain. Capture anything from your phone, scripts, webhooks or Discord into one typed graph. Arrange it on a visual canvas with buckets, tags and typed links. Let deterministic, hard-capped AI tasks summarize it, sort it and remind you about it.

**Runs on:** Cloudflare Workers, Durable Objects, Queues and AI Gateway, plus SurrealDB on Surreal Cloud and OpenRouter. There are no servers to run.

- **Everything is an item.** To-dos, hold-that-thoughts, reminders, buckets, task definitions and their runs are all items. Plugins define each type's schema *and* its interactive renderers.
- **Captures are never lost.** They're queued as soon as they arrive and written to the database afterwards.
- **AI is boxed in.**
  - You define every task: its trigger, inputs, model list and budget.
  - A separate gateway Worker with no public URL is the only path to models. It reaches OpenRouter through Cloudflare AI Gateway, which keeps the OpenRouter key in its own keystore, so no Worker ever holds the key.
  - Its spend caps are compiled into the build, so changing them takes a deploy.

## Quick start

```sh
make test                      # 50 native tests, no cloud needed
make db                        # local SurrealDB
cp workers/api/.dev.vars.example workers/api/.dev.vars
cp workers/gateway/.dev.vars.example workers/gateway/.dev.vars
make dev-gateway & make dev-api & make web
```

To deploy to Cloudflare, see [docs/deploy.md](docs/deploy.md).

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
