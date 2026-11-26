# 0007: AI output becomes proposed ops through Rust converters

- **Status:** accepted
- **Date:** 2026-11-23

## Context
Model output is untrusted and sometimes wrong (for example, made-up dates).

## Decision
- AI capability output must validate against its JSON Schema.
- A Rust converter (`ai_apply`) maps it to a whitelisted set of op kinds.
- The core validates the ops: targets exist, `fire_at` is in the future, per-day reminder caps hold.
- The ops are then applied (`apply: auto`) or turned into `suggested` items and edges (`apply: review`).

## Consequences
- Bad output fails safely without applying anything.
- Each new AI capability needs a converter written in Rust. This is intentional.
