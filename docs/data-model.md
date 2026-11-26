# Data model

The Rust types are in `crates/cairn-domain`. The SurrealDB tables are in `schema/`.

## Item

| Field | Type | Notes |
|---|---|---|
| `id` | ULID | Also used as the SurrealDB record key: `item:⟨ULID⟩` |
| `type` | string | Plugin type name, e.g. `todo` |
| `type_version` | int | The plugin schema version this payload conforms to |
| `title` | string | Shown on every renderer. Plugins may derive it. |
| `payload` | object | Owned by the plugin and validated against its JSON Schema |
| `tags` | string[] | Normalized to lowercase kebab-case |
| `lifecycle` | `active` \| `archived` \| `trashed` | See [lifecycle-gc.md](lifecycle-gc.md) |
| `pinned` | bool | Exempt from every automatic lifecycle rule |
| `source` | object | `{ kind: rest\|webhook\|discord\|ui\|task, ref?, label? }` |
| `created_at` / `updated_at` | datetime | |
| `archived_at` / `trashed_at` | datetime? | |

## Edge

Edges are stored in a SurrealDB `RELATION` table (`link`), so you can traverse the graph with `->link->item`.

| Field | Type | Notes |
|---|---|---|
| `id` | ULID | |
| `in` / `out` | item record | Direction is from `in` to `out` |
| `kind` | string | See the core kinds below. Plugins may declare more. |
| `props` | object | Free-form, e.g. `{ weight: 0.8 }` or `{ reason: "…" }` |
| `created_at` | datetime | |

Core edge kinds:

| Kind | Meaning |
|---|---|
| `contains` | bucket → member |
| `relates` | generic, undirected in meaning |
| `blocks` | A must be done before B |
| `derived-from` | summary → sources, or a split item → its original |
| `produced-by` | task-run → task, or output → task-run |
| `after` | task → task it depends on (scheduler chaining) |
| `remind-about` | reminder → item |
| `suggested` | an edge an AI task proposed that is still waiting for review |

## View

| Field | Type | Notes |
|---|---|---|
| `id` | ULID | |
| `kind` | `canvas` \| `board` \| `table` \| `calendar` | |
| `name` | string | |
| `query` | Query | The items this view shows (see below) |
| `layout` | map<ItemId, {x, y, w?, h?, collapsed?}> | Used by canvas and board views only |
| `config` | object | Per-kind settings: columns, sort order, grouping |

Buckets come in two kinds. A **manual bucket** is an item of type `bucket` with `contains` edges to its members. A **smart bucket** is a `bucket` item whose payload holds a `query`, so its members are computed and it has no `contains` edges.

## Op (ops log)

Ops are append-only. A SurrealDB event (`op_append_only` in `schema/0001_init.surql`) throws on any `DELETE`, and on any `UPDATE` other than GC redaction, which clears `before` and `after` and changes nothing else.

```
{ id, at, actor: user|task-run:<id>|gc|ingest:<source>, kind, target, before?, after? }
kind ∈ item.create | item.patch | item.lifecycle | edge.create | edge.delete |
       view.patch | item.purge
```
When GC purges an item it writes an `item.purge` tombstone op. Older ops for that item get their `before` and `after` cleared, so the deleted content doesn't survive in the history.

## Query

One small query language is shared by views, smart buckets, task inputs and the API:

```json
{
  "types": ["todo"],
  "tags_all": ["home"], "tags_any": [], "tags_none": ["someday"],
  "lifecycle": ["active"],
  "where": [{ "field": "due", "op": "lt", "value": "+2d" }],
  "text": "dmv",
  "related_to": { "item": "01J…", "kind": "contains", "depth": 1 },
  "updated_since": "-24h",
  "sort": [{ "field": "due", "dir": "asc" }],
  "limit": 200
}
```
`where` and `sort` may only use core fields or fields a plugin lists in `index`. Relative times (`-24h`, `+2d`) are resolved against the scheduler's clock, so dry runs give the same result every time.
