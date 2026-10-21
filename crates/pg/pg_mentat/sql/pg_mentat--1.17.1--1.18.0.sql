-- pg_mentat 1.17.1 -> 1.18.0 upgrade.
--
-- Each section below belongs to one change; add yours as a new section.

-- ---------------------------------------------------------------------------
-- Pull on a historical basis: edn_pull(pattern, entity_id, options).
--
-- A new 3-argument overload; the 2-argument edn_pull is unchanged. `options`
-- takes edn_q's temporal keys ({"asOf": T} / {"since": T}). edn_q's find-pull
-- calls it under an asOf/since query, so a pull in a historical query reads
-- that basis (before 1.18.0 it returned current values). Same shape as the
-- pgrx-generated install SQL.
-- ---------------------------------------------------------------------------
CREATE FUNCTION "edn_pull"("pattern" TEXT, "entity_id" bigint, "options" jsonb) RETURNS jsonb
STRICT LANGUAGE c
AS 'MODULE_PATHNAME', 'mentat_pull_with_options_wrapper';
