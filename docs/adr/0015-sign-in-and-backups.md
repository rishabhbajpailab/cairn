# 0015: A GitHub sign-in in front of Cairn, and weekly off-site backups

- **Status:** accepted
- **Date:** 2026-11-27

## Context

Since ADR 0014, Cairn has no Cloudflare Access in front, so its pages and API relied on Cairn's own bearer tokens alone. It shares EoC's SurrealDB, which EoC treats as disposable by design, but Cairn's namespace can't be rebuilt from anything. Both were open questions in EoC's `docs/DECISIONS.md`. The author answered them on 2026-11-27:

1. Cairn gets its own GitHub sign-in, with a separate OAuth App and its own secrets.
2. Cairn's namespace is backed up weekly to S3 or R2.

## Decision

### Sign-in

- A third container, **`cairn-sign-in`** (oauth2-proxy v7.15.6, the same as EoC's), is the only way in. With EoC, the door reaches it over `eoc-cairn-door`. Standalone, it's published on `127.0.0.1:8787`. `cairn` itself publishes nothing and is on no network the door is on.
- Its config is `deploy/sign-in.cfg` (committed, no secrets). The client ID, redirect URL and the one allowed GitHub account are in `deploy/sign-in.env`, and the client and cookie secrets are files in `deploy/secrets/` (all gitignored).
- Pages need the GitHub sign-in. The app's own API (`^/api/`, `/hub`) gets a 401 instead of a redirect when not signed in, and still needs Cairn's token as well.
- **Machine routes** bypass the sign-in, because phones, scripts, webhooks and Discord can't sign in. Each checks its own credential in Cairn. The list is `cairn_app::api::MACHINE_ROUTES`: `POST /api/v1/capture`, `POST /ingest/webhook/{source}`, `POST /integrations/discord/interactions` and `GET /healthz`. A test (`hosts/server/tests/sign_in_routes.rs`) fails if `sign-in.cfg`'s `skip_auth_routes` differ from that list, or if a listed route isn't in the router.
- Tried against oauth2-proxy v7.15.6, built from source, in front of the real `cairn`:
  - pages redirect to GitHub;
  - the API and `/hub` get 401, even with a valid Cairn token;
  - capture with a token passes through to a 202, with Cairn seeing the token; without one, Cairn answers 401;
  - webhooks, Discord and health reach Cairn;
  - `capture/../query`, encoded slashes and uppercase paths are not bypassed;
  - `api_routes` matches the path *and query string*, so `/hub` needs `^/hub(\?|$)`. `skip_auth_routes` matches the path only, so a query string can't widen a bypass.

  Not tried: a real GitHub login, and the WebSocket through the proxy with a session.

### Backups

- `deploy/backup.sh`, weekly from the host's crontab:
  - runs `surreal export` inside EoC's `index` as Cairn's own database user (no root);
  - checks that the export defines Cairn's tables, then compresses it;
  - uploads it with rclone, in a container, to S3 or R2 (`deploy/backup.env`), with `--no-check-dest --s3-no-head` so a write-only key works;
  - prunes local copies after 35 days. The bucket's own lifecycle rule handles remote ones.
- Exports skip users (`--users false`). SurrealDB redacts password hashes in exports (`PASSHASH '[REDACTED]'`), so a restored user would have a broken password. Importing that line also needs more than EDITOR. The user comes from `surreal-init.surql`.
- **Restore** goes through `surreal sql` as Cairn's user, in a throwaway container. `surreal import` (the `/import` endpoint) requires OWNER, which Cairn's user isn't. `surreal sql` writes everything it's given to `history.txt`, which is why it runs in a throwaway container.
- Tried end to end against SurrealDB 4.0.1 and rclone 1.68.2 (rclone's local backend, not a real bucket): back up, wipe, run the init, restore. Items, ops log, captures, the migration record and the `op_append_only` event all came back. A wrong password, an empty export or a missing destination each fail without uploading anything.

## Consequences

- Three containers instead of two. oauth2-proxy has a 128 MB limit; its real use isn't measured yet.
- A new machine route (a new webhook-like endpoint) has to be added to `MACHINE_ROUTES` *and* `sign-in.cfg`, or the sign-in blocks it. The test makes forgetting one of them a failure.
- The backup holds everything in Cairn. The bucket must be private, and the key should be scoped to that one bucket.
- The spool and ledger volumes are not in the weekly backup: the spool is normally empty, and the ledger is spend history. The VPS provider's server snapshots cover them.
