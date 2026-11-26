# Lifecycle, archive and garbage collection

```
 active ──archive──▶ archived ──trash──▶ trashed ──(retention elapsed)──▶ purged
   ▲                    │                   │
   └─────unarchive──────┘◀────restore───────┘
```

| State | Default views | Search | Edges | Restorable |
|---|---|---|---|---|
| `active` | shown | yes | live | n/a |
| `archived` | hidden (toggle "show archived") | yes, with `lifecycle: [archived]` | live, still traversable | always |
| `trashed` | hidden | Trash view only | hidden, kept | until `trash_retention` elapses |
| purged | gone | no | deleted (cascade) | no |

**Archive is permanent storage.** Use it for anything finished that you might want to look back at. **Trash is for actual deletion,** with a grace period.

## Rules GC follows

1. **GC only purges items that are already trashed.** Automatic policies may archive or trash items. A purge happens only after `trash_retention` has passed since `trashed_at`.
2. **Pinned items are untouchable.** The same goes for anything tagged `keep`. Every policy skips them.
3. **A purge cascades** to the item's edges, the layout entries in every view, and the item's index entries. The ops log keeps an `item.purge` tombstone with the id, type and time, and clears the `before` and `after` of older ops for that item.
4. **Nothing that gets referenced is purged by accident.** If a trashed item has inbound `contains` or `remind-about` edges from *active* items, GC skips it and reports it, rather than leaving the graph broken.
5. **GC is a task** (`builtin/gc`). It has a dry run, its reports are `task-run` items, and it uses no LLM. It runs from the TASKS queue like any other task.
6. **Never GC'd:** the gateway ledger, `task` items, `bucket` items, and anything created in the UI that has no policy covering its type.

## Default policies (`config/cairn.toml` → `[gc]`, deployed with the API Worker)

```toml
[gc]
trash_retention = "30d"
capture_retention = "7d" # raw envelopes in the `capture` table, once resolved into an item

[[gc.policy]]            # completed to-dos drop out of sight but stay searchable
match = { types = ["todo"], where = [{ field = "done", op = "eq", value = true }] }
older_than = "14d"       # measured from updated_at
action = "archive"

[[gc.policy]]            # reminders that fired and were acknowledged
match = { types = ["reminder"], where = [{ field = "state", op = "eq", value = "acked" }] }
older_than = "7d"
action = "archive"

[[gc.policy]]            # stale hold-that-thoughts ask before archiving (see below)
match = { types = ["hold-that-thought"], tags_none = ["keep"] }
older_than = "60d"
action = "nudge"
```

Task-run retention is set per task (`retain_runs: 30d`). Expired runs are **trashed**, and so are their outputs, but only if those outputs have no edges other than `produced-by`. A digest you linked to something stays.

**`nudge`** doesn't change state. It adds the item to the next morning digest with **Keep / Archive / Trash** buttons in Discord. This is how forgotten thoughts come back up instead of disappearing quietly.
