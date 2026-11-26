# 0005: Task definitions are items on the graph

- **Status:** accepted
- **Date:** 2026-11-23

## Context
Task definitions could live in a git repo as YAML or in the store as items.

## Decision
They are items of type `task`. Runs are `task-run` items, linked to their task with `produced-by` edges and to their outputs the same way. YAML is used for import and export only.

## Consequences
- The pipeline can be seen and edited on the canvas, outputs are linked to it automatically, and history comes from the ops log.
- There's no git review for task changes. To compensate, there's a nightly `builtin/export` and `cairn task export`, so you can commit snapshots if you want them.
