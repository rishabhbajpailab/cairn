# Plugins

There are two kinds of plugin. Both live in `plugins/`, and each one is a folder with a `plugin.json` file. Workers have no filesystem, so the whole `plugins/` directory is **compiled into the API Worker** (`include_dir!`), and `web/scripts/copy-plugins.mjs` copies each `views.js` into the static assets. Adding or changing a plugin therefore means a deploy.

```
plugins/
  types/<name>/   plugin.json  schema.json  views.js  [migrations.json]
  ai/<name>/      plugin.json  prompt.md    output.schema.json
```

## Type plugins

`plugin.json`:
```json
{
  "kind": "type",
  "name": "todo",
  "version": 1,
  "title": "To-do",
  "icon": "check-square",
  "schema": "schema.json",
  "index": { "due": "datetime", "done": "bool", "priority": "int" },
  "title_from": "text",
  "edge_kinds": ["subtask-of"],
  "capture": [{ "prefix": "todo:", "field": "text" }, { "prefix": "t:", "field": "text" }],
  "actions": {
    "complete": { "patch": { "done": true }, "discord_button": "Done" },
    "snooze_1d": { "snooze": "+1d", "discord_button": "Tomorrow" }
  },
  "views": "views.js"
}
```

- **The schema is JSON Schema**, the single source of truth. The Rust core validates against it, and the TypeScript types in `web/` are generated from it (`make types`).
- **`index`** lists the payload fields exposed to `where`, `sort` and views. The core copies these into `item.idx.*` on write, so SurrealDB can index them without knowing about payloads.
- **`capture` matchers** are declarative only: prefix, regex, or JSON-shape match. Plugins never run code during ingest.
- **`actions`** are declarative patches or lifecycle operations. They run identically whether triggered from the UI, from Discord buttons, or as AI-proposed ops.
- **Migrations:** when `version` goes up, add a step to `migrations.json`, such as `{ "from": 1, "steps": [{ "rename": ["text", "body"] }, { "default": ["priority", 0] }] }`. The available steps are `rename`, `default`, `drop`, `map` (enum remap) and `move` (into a nested object). Migrations are **declarative on purpose**, because the core is Rust and doesn't embed a JS runtime. The core runs them lazily on read, or in bulk through an admin endpoint (`POST /api/v1/admin/migrate-type/{type}`, M2). For anything more complicated, write a one-off script against the REST API.

### `views.js` contract (framework-agnostic)

Each renderer is a plain function that receives a DOM element and returns update and destroy hooks. They're served from `/plugins/types/<name>/views.js`. The web app doesn't care what framework, if any, a plugin uses inside it.

```js
/** @type {import('../../../web/src/lib/plugin-api').TypeViews} */
export default {
  card(el, item, ctx)   { /* compact; boards, lists, search results */ return { update(item) {}, destroy() {} } },
  node(el, item, ctx)   { /* canvas node */ },
  detail(el, item, ctx) { /* full read view */ },
  editor(el, item, ctx) { /* form; call ctx.patch({...}) to save */ },
}
```
`ctx` gives you `patch(partial)`, `action(name)`, `link(toId, kind)`, `navigate(id)` and `theme`. Any renderer you leave out falls back to a generic JSON renderer.

Plugins are trusted code and are **not sandboxed** (single user, your own repo). Revisit this if plugins ever come from anyone else.

## AI capability plugins

An AI capability is a prompt plus an output contract. It doesn't decide when it runs, what it runs on, or which model it uses. Task items decide all of that (see [tasks.md](tasks.md)).

```json
{
  "kind": "ai",
  "name": "triage",
  "version": 1,
  "prompt": "prompt.md",
  "input": { "shape": "items", "fields": ["title", "payload", "tags", "source"] },
  "output": "output.schema.json",
  "apply": "ops.triage"
}
```

- **`apply`** names a built-in, deterministic Rust converter in `cairn-app::ai_apply`. It turns validated output into proposed ops. Converters are code-reviewed Rust and are never generated from model output.
- The output must validate against `output.schema.json`, or the run fails and **nothing is applied**.
- Allowed op kinds are whitelisted per converter. For example, `triage` can set `type`, tags and buckets, but it can't trash or purge anything.
