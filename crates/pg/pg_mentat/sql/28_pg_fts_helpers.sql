-- pg_mentat <-> pg_fts integration helpers.
--
-- pg_fts (https://codeberg.org/gregburd/pg_fts, PostgreSQL license) is a
-- BM25/BM25F full-text index access method: a dedicated `fts` index AM that
-- keeps the corpus statistics BM25 needs and answers ranking from the index
-- with no heap recheck. It is a SOFT dependency; nothing here forces it to be
-- installed, and pg_mentat's built-in :db/fulltext (native tsvector/GIN) keeps
-- working when pg_fts is absent.
--
-- pg_fts requires PostgreSQL 17+. The helpers check at call time and raise a
-- clear :db.error/* with install instructions if it is missing.
--
-- Query shape once the index exists (issue #332):
--
--   CREATE INDEX ... USING fts (to_ftsdoc('english', <col>))
--   ... WHERE to_ftsdoc('english', <col>) @@@ to_ftsquery('english', 'quick & fox')
--       ORDER BY to_ftsdoc('english', <col>) <=> to_ftsquery('english', 'quick fox')
--
-- Reference: https://codeberg.org/gregburd/pg_fts

-- mentat.has_pg_fts(): true if the pg_fts extension is installed here.
CREATE OR REPLACE FUNCTION mentat.has_pg_fts()
RETURNS boolean
LANGUAGE sql STABLE
AS $$
    SELECT EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'pg_fts');
$$;

-- mentat.create_fts_index(attr_ident text, regconfig text DEFAULT 'english')
--
-- Create a BM25 pg_fts index on the text values of a :db.type/string
-- attribute, scoped to the default store (store_id = 0) and live datoms
-- (added = TRUE). Once it exists, a Datalog query can rank matches with the
-- (bm25-match $ :attr "query" k) where-fn, which maps to a `@@@` predicate
-- plus a `<=>` ordering against the stored text and joins the rest of the
-- clause graph back through the entid column.
--
-- Idempotent (CREATE INDEX IF NOT EXISTS). Returns the index name.
--
-- Errors with :db.error/* if pg_fts is absent, the attribute is unknown, or
-- the attribute is not :db.type/string (BM25 ranks text only).
CREATE OR REPLACE FUNCTION mentat.create_fts_index(
    attr_ident TEXT,
    regconfig  TEXT DEFAULT 'english'
)
RETURNS TEXT
LANGUAGE plpgsql VOLATILE
AS $$
DECLARE
    attr_entid BIGINT;
    attr_type  TEXT;
    idx_name   TEXT;
BEGIN
    IF NOT mentat.has_pg_fts() THEN
        RAISE EXCEPTION USING
            ERRCODE = 'feature_not_supported',
            MESSAGE = ':db.error/missing-extension pg_fts is not installed in this database. ' ||
                      'pg_fts is an OPTIONAL BM25 full-text index for pg_mentat (PostgreSQL 17+). ' ||
                      'To enable: install pg_fts per https://codeberg.org/gregburd/pg_fts, then ' ||
                      'run CREATE EXTENSION pg_fts; before retrying. Without it, :db/fulltext ' ||
                      'still works via the built-in tsvector/GIN path.';
    END IF;

    SELECT s.entid, s.value_type::text
      INTO attr_entid, attr_type
      FROM mentat.schema s
     WHERE s.ident = attr_ident;

    IF attr_entid IS NULL THEN
        RAISE EXCEPTION USING
            ERRCODE = 'undefined_object',
            MESSAGE = ':db.error/attribute-not-found Attribute ' || quote_literal(attr_ident) ||
                      ' is not defined in mentat.schema. Define it first with ' ||
                      ':db/valueType :db.type/string.';
    END IF;

    IF attr_type <> 'string' THEN
        RAISE EXCEPTION USING
            ERRCODE = 'invalid_parameter_value',
            MESSAGE = ':db.error/wrong-type-for-fts-index Attribute ' || quote_literal(attr_ident) ||
                      ' has value type ''' || attr_type || ''' but pg_fts indexes only ' ||
                      ':db.type/string.';
    END IF;

    idx_name := 'mentat_fts_' || attr_entid;
    EXECUTE format(
        'CREATE INDEX IF NOT EXISTS %I ON mentat.datoms_text_new ' ||
        'USING fts (to_ftsdoc(%L, v)) WHERE a = %s AND store_id = 0 AND added',
        idx_name, regconfig, attr_entid
    );
    RETURN idx_name;
END;
$$;

COMMENT ON FUNCTION mentat.has_pg_fts() IS
    'True if the optional pg_fts BM25 full-text extension is installed.';
COMMENT ON FUNCTION mentat.create_fts_index(TEXT, TEXT) IS
    'Create a BM25 pg_fts index on a :db.type/string attribute (optional; needs pg_fts).';
