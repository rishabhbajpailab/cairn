-- One-time setup of Cairn's namespace in EoC's SurrealDB (ADR 0014; docs/deploy.md, step 2).
-- Don't edit the password in here: it is substituted on the way in, through a throwaway
-- container, because `surreal sql` writes everything it is given to a history.txt:
--   sed "s/CHANGE_ME/$CAIRN_PASS/" deploy/surreal-init.surql | docker run --rm -i \
--     --network eoc-cairn-index surrealdb/surrealdb:v4.0.1 sql --endpoint http://index:8000 \
--     --user root --pass … --hide-welcome
-- Cairn gets its own namespace and a database-level user, so it can't see EoC's `eoc`
-- namespace (and EoC's users can't see Cairn's unless they are root).
DEFINE NAMESPACE IF NOT EXISTS cairn;
USE NS cairn;
DEFINE DATABASE IF NOT EXISTS main;
USE NS cairn DB main;
-- EDITOR can define tables, fields, indexes and events (needed by `cairn migrate`) but not
-- users or access methods.
DEFINE USER IF NOT EXISTS cairn ON DATABASE PASSWORD 'CHANGE_ME' ROLES EDITOR;
