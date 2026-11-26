# 0008: Cloudflare Workers + Durable Objects as the runtime

- **Status:** accepted (supersedes 0002)
- **Date:** 2026-11-23

## Context
The tool is meant to be cloud-native, with nothing on your own hardware. It is single-user and bursty, and it needs scheduled work, a durable intake, and push updates to a browser.

## Decision
- **API Worker** (`workers/api`), written in Rust with workers-rs and running Axum. It hosts:
  - the REST, webhook and Discord routes
  - the CAPTURES and TASKS queue consumers
  - an hourly cron backstop
  - the **Scheduler** DO (alarm-based scheduling, ADR 0010)
  - the **Hub** DO (WebSocket fan-out, replacing database live queries)
- **Gateway Worker** (`workers/gateway`) plus the **Ledger** DO (ADR 0003).
- The web app and plugin `views.js` are served as **Workers static assets**, behind **Cloudflare Access**.
- All logic lives in platform-agnostic libraries (`cairn-app`, `cairn-gateway-core`), tested natively. The Worker crates are thin glue.

## Consequences
- No servers to patch, the system scales to zero, and deploys are atomic per Worker.
- WebAssembly constraints: no tokio runtime, no filesystem (plugins, schema and config are embedded at build time), randomness through getrandom's `wasm_js` feature, HTTP-only database access (ADR 0011).
- Changing config, plugins or caps requires a deploy. That's a feature for caps, and a minor cost for plugins.
