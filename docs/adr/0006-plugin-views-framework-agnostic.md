# 0006: Plugin views are framework-agnostic `mount` functions

- **Status:** accepted
- **Date:** 2026-11-23

## Context
Item types are interactive renderers, not just data definitions. The web app uses Svelte, but plugins shouldn't be locked to it.

## Decision
`views.js` default-exports `{card, node, detail, editor}`. Each is a function `(el, item, ctx) => {update, destroy}` and is loaded with `import()` from `/plugins/types/<name>/views.js` (Workers static assets). Schemas are JSON Schema, so the Rust core can validate them.

## Consequences
- Any plugin can use vanilla DOM, Svelte, Lit or anything else inside its renderers.
- There is no sandbox, because plugins are trusted code.
- TypeScript types are generated from the JSON Schema.
