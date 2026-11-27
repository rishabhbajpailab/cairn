-- One-time setup on the shared SurrealDB (ADR 0014). Run as root on the VPS, e.g.
--   surreal sql --endpoint http://127.0.0.1:8000 --user root --pass … < deploy/surreal-init.surql
-- Cairn gets its own namespace and a database-level user, so it can't see EoC's data
-- (and EoC's users can't see Cairn's unless they are root).
-- Replace CHANGE_ME with a long random password, then put the same value in deploy/cairn.env.
DEFINE NAMESPACE IF NOT EXISTS cairn;
USE NS cairn;
DEFINE DATABASE IF NOT EXISTS main;
USE NS cairn DB main;
-- EDITOR can define tables, fields, indexes and events (needed by `cairn migrate`) but not
-- users or access methods.
DEFINE USER IF NOT EXISTS cairn ON DATABASE PASSWORD 'CHANGE_ME' ROLES EDITOR;
