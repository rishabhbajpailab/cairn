# Ingest

Every adapter ends up in the same place: `auth → pure resolve → spool → consumer → store`. Once auth passes, a capture is never lost (invariant 1). It's durable as soon as it's committed to the SQLite spool on the `cairn` volume, even if SurrealDB is down or the process restarts.

The reply comes back before the item is written:

```json
202 { "capture_id": "01J…", "item_id": "01J…", "type": "todo" }
```

`item_id` is the planned id. The item appears once the consumer runs, usually within seconds, and open browsers refresh through the Hub. If the spool itself can't be written (disk full, volume missing), the reply is `503 spool_unavailable` and the client should retry. That's the only way a capture can fail.

## Auth

API tokens are stored hashed (SHA-256) and each one carries scopes.

**M0 (now):** tokens are listed in `CAIRN_TOKENS` (`deploy/cairn.env`):

```
CAIRN_TOKENS="phone:capture:<sha256>;ui:write:<sha256>;me:admin:<sha256>"
```

Run `make hash` to get the hash. **M1:** tokens move to the `api_token` table, managed through admin endpoints.

| Scope | Can do | Typical use |
|---|---|---|
| `capture` | `POST /api/v1/capture` only | iOS Shortcut, scripts, cron jobs |
| `read` | GET endpoints | dashboards, newsletter pipelines |
| `write` | everything except admin (includes `read` and `capture`) | the web UI (behind the GitHub sign-in) |
| `admin` | `POST /api/v1/admin/migrate`, token management, GC purge-now | you, from `make` targets |

## REST capture

```http
POST /api/v1/capture
Authorization: Bearer <capture-scoped token>
Content-Type: application/json

{ "text": "todo: renew registration before Dec 15", "tags": ["car"], "type": null, "meta": {} }
```
The endpoint also accepts `text/plain` (the whole body becomes `text`). It returns `202` as described above.

**iOS Shortcut:** use *Ask for Input* → *Get Contents of URL* (POST, JSON, header `Authorization`). Put it on the Action Button or the lock screen.

## Webhooks

`POST /ingest/webhook/:source` with the header `X-Cairn-Token`. Each source is a `[[webhook]]` entry in `config/cairn.toml`:

```toml
[[webhook]]
source = "github"
token_sha256 = "…"
map = { title = "$.issue.title", text = "$.issue.body", tags = ["github"], type = "research" }
```
Mapping uses JSONPath. If mapping fails, the raw capture is still stored, tagged `unmapped`.

## Discord

**HTTP interactions endpoint:** `POST /integrations/discord/interactions` on `cairn`. It needs no long-running connection.
- Verify `X-Signature-Ed25519` and `X-Signature-Timestamp` against `DISCORD_PUBLIC_KEY`, and reject on failure. Discord sends deliberately invalid requests to test this.
- Respond `{type: 1}` to PING (type 1).
- Slash commands:
  - `/c <text>`: capture
  - `/todo <text> [due]`
  - `/remind <text> <when>`
  - `/find <query>`
  - `/ai status|off` (there is no `on`; re-enabling requires a gateway deploy, see spend-control.md)
- Message context command **"Save to Cairn"** captures the message text and its link.
- Components (buttons) carry a `custom_id` of the form `act:<item_id>:<action>` and run plugin actions.

**Notifier:** the scheduler timer sends reminders through the Discord REST API (`create DM` → `create message`) using `DISCORD_BOT_TOKEN`. Reminder messages include buttons for the type's `actions` that set a `discord_button` label. Quiet hours (`[notify].quiet_hours`) hold a reminder until the window ends.

**Optional, later:** free-text DMs with no slash command need a long-running gateway (WebSocket) client with the message-content intent. On the VPS, `cairn` is a persistent process, so that client could run inside it as another background task. It would post captures through the same `ingest` path. Not built yet.
