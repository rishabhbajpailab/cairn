-- Cairn schema v1. Targets SurrealDB 3.x/4.x (matches the surrealdb 4.x SDK).
-- Embedded in the API Worker and applied in filename order by POST /api/v1/admin/migrate
-- (`make migrate`). Applied files are recorded in _migration.
-- Record keys are ULIDs: item:⟨01J…⟩

-- ───────────── items ─────────────
DEFINE TABLE item SCHEMAFULL;
DEFINE FIELD type         ON item TYPE string;
DEFINE FIELD type_version ON item TYPE int DEFAULT 1;
DEFINE FIELD title        ON item TYPE string DEFAULT "";
DEFINE FIELD payload      ON item TYPE object FLEXIBLE DEFAULT {};
-- idx: plugin-declared index fields copied out of payload by cairn-plugins (see docs/plugins.md)
DEFINE FIELD idx          ON item TYPE object FLEXIBLE DEFAULT {};
DEFINE FIELD tags         ON item TYPE array<string> DEFAULT [];
DEFINE FIELD lifecycle    ON item TYPE string DEFAULT "active"
    ASSERT $value IN ["active", "archived", "trashed"];
DEFINE FIELD pinned       ON item TYPE bool DEFAULT false;
DEFINE FIELD source       ON item TYPE object FLEXIBLE DEFAULT { kind: "ui" };
DEFINE FIELD created_at   ON item TYPE datetime DEFAULT time::now() READONLY;
DEFINE FIELD updated_at   ON item TYPE datetime VALUE time::now();
DEFINE FIELD archived_at  ON item TYPE option<datetime>;
DEFINE FIELD trashed_at   ON item TYPE option<datetime>;

DEFINE INDEX item_type_lifecycle ON item FIELDS type, lifecycle;
DEFINE INDEX item_tags           ON item FIELDS tags;
DEFINE INDEX item_updated        ON item FIELDS updated_at;

DEFINE ANALYZER cairn_text TOKENIZERS blank, class FILTERS lowercase, ascii, snowball(english);
DEFINE INDEX item_title_ft ON item FIELDS title FULLTEXT ANALYZER cairn_text BM25;

-- ───────────── edges ─────────────
DEFINE TABLE link TYPE RELATION IN item OUT item SCHEMAFULL;
DEFINE FIELD kind       ON link TYPE string;
DEFINE FIELD props      ON link TYPE object FLEXIBLE DEFAULT {};
DEFINE FIELD created_at ON link TYPE datetime DEFAULT time::now() READONLY;
DEFINE INDEX link_kind   ON link FIELDS kind;
DEFINE INDEX link_unique ON link FIELDS in, out, kind UNIQUE;

-- ───────────── views ─────────────
DEFINE TABLE view SCHEMAFULL;
DEFINE FIELD kind       ON view TYPE string ASSERT $value IN ["canvas", "board", "table", "calendar"];
DEFINE FIELD name       ON view TYPE string;
DEFINE FIELD query      ON view TYPE object FLEXIBLE DEFAULT {};
-- layout: { "<item ulid>": { x, y, w?, h?, collapsed? } }
DEFINE FIELD layout     ON view TYPE object FLEXIBLE DEFAULT {};
DEFINE FIELD config     ON view TYPE object FLEXIBLE DEFAULT {};
DEFINE FIELD updated_at ON view TYPE datetime VALUE time::now();

-- ───────────── raw captures ─────────────
DEFINE TABLE capture SCHEMAFULL;
DEFINE FIELD source       ON capture TYPE object FLEXIBLE;           -- { kind, ref?, label? }
DEFINE FIELD content_type ON capture TYPE string DEFAULT "application/json";
DEFINE FIELD body         ON capture TYPE any;                       -- raw, never rejected
DEFINE FIELD received_at  ON capture TYPE datetime DEFAULT time::now() READONLY;
DEFINE FIELD resolved     ON capture TYPE bool DEFAULT false;
DEFINE FIELD item         ON capture TYPE option<record<item>>;
DEFINE FIELD error        ON capture TYPE option<string>;
DEFINE INDEX capture_unresolved ON capture FIELDS resolved, received_at;

-- ───────────── ops log (append-only) ─────────────
DEFINE TABLE op SCHEMAFULL;
DEFINE FIELD at     ON op TYPE datetime DEFAULT time::now() READONLY;
DEFINE FIELD actor  ON op TYPE string;          -- user | task-run:<ulid> | gc | ingest:<source>
DEFINE FIELD kind   ON op TYPE string;
DEFINE FIELD target ON op TYPE string;          -- "item:<ulid>" | "link:<ulid>" | "view:<ulid>"
DEFINE FIELD batch  ON op TYPE string;          -- ulid shared by ops applied together (undo unit)
DEFINE FIELD before ON op TYPE option<object> FLEXIBLE;
DEFINE FIELD after  ON op TYPE option<object> FLEXIBLE;
DEFINE INDEX op_target ON op FIELDS target, at;
DEFINE INDEX op_batch  ON op FIELDS batch;

-- Append-only guard. The only update allowed is GC redaction: clearing before/after
-- while every other field stays the same. Deletes are never allowed.
DEFINE EVENT op_append_only ON op
    WHEN $event = "DELETE"
      OR ($event = "UPDATE" AND (
            $after.before != NONE OR $after.after != NONE
         OR $after.kind != $before.kind OR $after.target != $before.target
         OR $after.actor != $before.actor OR $after.batch != $before.batch))
    THEN { THROW "ops log is append-only"; };

-- ───────────── API tokens (hashed, scoped) ─────────────
DEFINE TABLE api_token SCHEMAFULL;
DEFINE FIELD name       ON api_token TYPE string;
DEFINE FIELD sha256     ON api_token TYPE string;
DEFINE FIELD scopes     ON api_token TYPE array<string>
    ASSERT $value.all(|$s| $s IN ["capture", "read", "write", "admin"]);
DEFINE FIELD created_at ON api_token TYPE datetime DEFAULT time::now() READONLY;
DEFINE FIELD last_used  ON api_token TYPE option<datetime>;
DEFINE FIELD revoked    ON api_token TYPE bool DEFAULT false;
DEFINE INDEX api_token_sha ON api_token FIELDS sha256 UNIQUE;

-- ───────────── scheduler bookkeeping ─────────────
-- Idempotency: one row per (task, window, input) run key. task-run *items* hold the human-facing record.
DEFINE TABLE run_key SCHEMAFULL;
DEFINE FIELD task       ON run_key TYPE record<item>;
DEFINE FIELD run_item   ON run_key TYPE option<record<item>>;
DEFINE FIELD state      ON run_key TYPE string ASSERT $value IN ["running", "succeeded", "failed", "skipped"];
DEFINE FIELD created_at ON run_key TYPE datetime DEFAULT time::now() READONLY;

-- ───────────── migrations ─────────────
DEFINE TABLE _migration SCHEMAFULL;
DEFINE FIELD applied_at ON _migration TYPE datetime DEFAULT time::now() READONLY;
