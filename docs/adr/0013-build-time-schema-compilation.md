# 0013: Compile plugin JSON Schemas at build time; plan on Workers Paid

- **Status:** accepted
- **Date:** 2026-11-26

## Context

Workers bill CPU time per request: 10 ms on the Free plan, up to 5 minutes on Paid. Waiting on the network doesn't count. The API Worker builds its plugin registry on the first request in each new isolate. With runtime compilation (`jsonschema::validator_for` per schema), that first load took **30–45 ms** of CPU as wasm under V8 (`make coldstart`, 8 schemas). Most of it came from checking each schema against the draft 2020-12 meta-schema, a validator `jsonschema` doesn't cache on wasm.

## Decision

- Upgrade `jsonschema` to 0.63 and add `cairn-plugins-embedded`. Its `build.rs` emits a `#[jsonschema::validator(path = …)]` for every schema a plugin manifest names, and its `Embedded` source hands those to the registry as `Compiled` function pointers. The registry then reads only the manifests and prompts at runtime.
- `cairn-plugins` keeps runtime compilation for `FsSource` and `insert_type` (native tests, tooling). `Registry::runtime_compiled()` reports which plugins took that path. The Worker logs a warning if any did, and a test asserts none do.
- A test checks that the generated validators agree with runtime ones on a fixed sample set.
- **Production runs on the Workers Paid plan.** Precompiling brings the cold registry load to 3–4.5 ms, but a whole cold request (instantiation, config, routing, JSON, Rust↔JS conversions, ed25519 for Discord) leaves little headroom under 10 ms.

## Consequences

- Changing a schema needs a rebuild. That was already true, because plugins are embedded.
- The API wasm grows by about 0.8 MB raw (about 190 KB gzipped) to 5.8 MB (1.57 MB gzipped), within both plans' size limits.
- Schema errors surface at `cargo build`, not at the first request.
- If generated and runtime validators ever disagree (a `jsonschema` bug), the parity test should catch it for the sampled instances. It is not a proof.
