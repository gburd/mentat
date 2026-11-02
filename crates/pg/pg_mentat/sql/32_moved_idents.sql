-- Idents moved by pg_mentat before 1.19.0 (#415): diagnosis and repair.
--
-- Before 1.19.0, re-asserting an existing ident in a map or list entity
-- without :db/id ([{:db/ident :color/red :db/doc "red"}]) allocated a new
-- entity N and pointed the store's idents row at it, while the :db/ident
-- datom and the rest of the map stayed on the ident's entity O. pg_mentat
-- then resolved the ident to N: values written after the move (a ref to
-- :color/red, [:db/add :color/red ...]) went to N, values written before are
-- on O, so the data split in two. Queries through the datom
-- ([?c :db/ident :color/red]) still found O. An attribute's ident also moved,
-- but its schema row kept O, so its data did not split.
--
-- mentat.moved_idents(store) lists such idents and changes nothing.
-- mentat.repair_moved_idents(store, dry_run) points each back at O and moves
-- what was written to N onto O; opt-in, and a dry run by default.

-- ---------------------------------------------------------------------------
-- mentat.moved_idents(store TEXT DEFAULT 'default')
--
-- One row per ident whose idents row names a different entity than the one
-- holding its live [O :db/ident ident] datom: the shape the bug left.
--   ident     the ident, ':color/red'
--   moved_to  N, the entid the idents row names
--   owner     O, the entity with the live :db/ident datom
--   n_datoms  live datoms with N as their entity
--   n_refs    live ref values that point at N
--   blocker   NULL if repair_moved_idents can repair it, else why it can't
--
-- A second re-assert moved the ident again, from N to a new N2, and left no
-- trace of N: refs written to N between the two moves are not found here.
-- ---------------------------------------------------------------------------
CREATE OR REPLACE FUNCTION mentat.moved_idents(store TEXT DEFAULT 'default')
RETURNS TABLE (
    ident    TEXT,
    moved_to BIGINT,
    owner    BIGINT,
    n_datoms BIGINT,
    n_refs   BIGINT,
    blocker  TEXT
)
LANGUAGE plpgsql STABLE
SET search_path = pg_catalog, pg_temp
AS $fn$
DECLARE
    s TEXT;
    sid BIGINT;
    rec RECORD;
    ty TEXT;
    n BIGINT;
    hit BIGINT;
    types TEXT[] := ARRAY['ref', 'boolean', 'long', 'double', 'instant',
                          'text', 'keyword', 'uuid', 'bytes'];
BEGIN
    SELECT st.schema_name, st.store_id INTO s, sid
      FROM mentat.stores st WHERE st.store_name = store;
    IF s IS NULL THEN
        RAISE EXCEPTION 'pg_mentat: no store named %', store;
    END IF;
    -- 10 is :db/ident; its values are stored without the leading ':'.
    FOR rec IN EXECUTE format(
        'SELECT i.ident, i.entid AS moved_to, k.e AS owner
           FROM %1$I.idents i
           JOIN %1$I.current_keyword k
             ON k.store_id = $1 AND k.a = 10 AND k.v = substr(i.ident, 2)
          WHERE k.e <> i.entid
          ORDER BY i.ident', s) USING sid
    LOOP
        ident := rec.ident;
        moved_to := rec.moved_to;
        owner := rec.owner;
        n_datoms := 0;
        blocker := NULL;
        EXECUTE format('SELECT count(*) FROM %I.current_ref WHERE store_id = $1 AND v = $2', s)
            INTO n_refs USING sid, rec.moved_to;
        FOREACH ty IN ARRAY types LOOP
            EXECUTE format('SELECT count(*) FROM %I.%I WHERE store_id = $1 AND e = $2',
                           s, 'current_' || ty)
                INTO n USING sid, rec.moved_to;
            n_datoms := n_datoms + n;
            -- An attribute on both entities: one entity would hold both
            -- values, two of a cardinality-one attribute.
            EXECUTE format(
                'SELECT min(x.a) FROM %1$I.%2$I x
                  WHERE x.store_id = $1 AND x.e = $2
                    AND EXISTS (SELECT 1 FROM %1$I.%2$I y
                                 WHERE y.store_id = $1 AND y.e = $3 AND y.a = x.a)',
                s, 'current_' || ty)
                INTO hit USING sid, rec.moved_to, rec.owner;
            IF hit IS NOT NULL AND blocker IS NULL THEN
                blocker := format('both entities have a value of attribute %s', hit);
            END IF;
            -- N as an attribute: its datoms would need N's schema row.
            EXECUTE format('SELECT count(*) FROM %I.%I WHERE store_id = $1 AND a = $2',
                           s, 'datoms_' || ty || '_new')
                INTO hit USING sid, rec.moved_to;
            IF hit > 0 AND blocker IS NULL THEN
                blocker := format('entity %s is used as an attribute', rec.moved_to);
            END IF;
        END LOOP;
        -- One entity refers to both through one attribute: two values merge
        -- into one.
        EXECUTE format(
            'SELECT min(x.e) FROM %1$I.current_ref x
               JOIN %1$I.current_ref y
                 ON y.store_id = x.store_id AND y.e = x.e AND y.a = x.a
              WHERE x.store_id = $1 AND x.v = $2 AND y.v = $3', s)
            INTO hit USING sid, rec.moved_to, rec.owner;
        IF hit IS NOT NULL AND blocker IS NULL THEN
            blocker := format('entity %s refers to both through one attribute', hit);
        END IF;
        EXECUTE format('SELECT count(*) FROM %I.idents WHERE entid = $1', s)
            INTO hit USING rec.owner;
        IF hit > 0 AND blocker IS NULL THEN
            blocker := format('entity %s has another ident in %I.idents', rec.owner, s);
        END IF;
        RETURN NEXT;
    END LOOP;
END;
$fn$;

-- ---------------------------------------------------------------------------
-- mentat.repair_moved_idents(store TEXT DEFAULT 'default',
--                            dry_run BOOLEAN DEFAULT true)
--
-- For each row of moved_idents(store) with no blocker: point the idents row
-- back at O, and move what was written to N onto O, in the log and the
-- current state: datoms with N as entity get O, ref values that point at N
-- point at O. Transaction ids stay as they are, so the history keeps when
-- each value was written; as-of a time after the move now shows the value on
-- O. One transaction can have written a datom on both: re-loading
-- [car :c/color :color/red] after the move retracted [car :c/color O] and
-- asserted [car :c/color N]. Those rows become one datom; the repair keeps
-- one row with the transaction's net effect (asserted if either row is), or
-- none if the datom already had that state (the pair above: car's color
-- stays O, asserted when it was first written). A row with a blocker is
-- skipped and left as it is; an ident whose repair fails is rolled back on
-- its own. Returns one row per ident with what was done ('repaired',
-- 'would repair', 'skipped: why', or 'failed: error').
--
-- Run it with nothing writing to the store: a writer that resolved the ident
-- before the repair committed can still write a ref to N. Back up first; run
-- it in your own transaction and check moved_idents(store) is empty before
-- COMMIT.
-- ---------------------------------------------------------------------------
CREATE OR REPLACE FUNCTION mentat.repair_moved_idents(
    store   TEXT    DEFAULT 'default',
    dry_run BOOLEAN DEFAULT true
)
RETURNS TABLE (ident TEXT, moved_to BIGINT, owner BIGINT, action TEXT)
LANGUAGE plpgsql
SET search_path = pg_catalog, pg_temp
AS $fn$
DECLARE
    s TEXT;
    sid BIGINT;
    rec RECORD;
    ty TEXT;
    vmap TEXT;
    vhit TEXT;
    repaired BOOLEAN := false;
    types TEXT[] := ARRAY['ref', 'boolean', 'long', 'double', 'instant',
                          'text', 'keyword', 'uuid', 'bytes'];
BEGIN
    SELECT st.schema_name, st.store_id INTO s, sid
      FROM mentat.stores st WHERE st.store_name = store;
    IF s IS NULL THEN
        RAISE EXCEPTION 'pg_mentat: no store named %', store;
    END IF;
    IF NOT dry_run THEN
        -- Wait for, and hold off, schema-changing transactions of the store
        -- (they lock this row first), and make every backend reload its idents.
        UPDATE mentat.cache_generation SET gen = gen + 1 WHERE store_name = store;
    END IF;
    FOR rec IN SELECT * FROM mentat.moved_idents(store) LOOP
        ident := rec.ident;
        moved_to := rec.moved_to;
        owner := rec.owner;
        IF rec.blocker IS NOT NULL THEN
            action := 'skipped: ' || rec.blocker;
        ELSIF dry_run THEN
            action := 'would repair';
        ELSE
          BEGIN
            EXECUTE format('UPDATE %I.idents SET entid = $1 WHERE ident = $2', s)
                USING rec.owner, rec.ident;
            FOREACH ty IN ARRAY types LOOP
                -- $2 is N, $3 is O. A ref's value moves too.
                vmap := CASE WHEN ty = 'ref' THEN 'CASE WHEN v = $2 THEN $3 ELSE v END' ELSE 'v' END;
                vhit := CASE WHEN ty = 'ref' THEN ' OR v IN ($2, $3)' ELSE '' END;
                -- Rows that become one datom in one transaction: keep the one
                -- with the net effect, or none if the datom had it already.
                EXECUTE format($q$
                    WITH m AS (
                        SELECT e, a, v, tx, added,
                               CASE WHEN e = $2 THEN $3 ELSE e END AS me, %3$s AS mv
                          FROM %1$I.%2$I
                         WHERE store_id = $1 AND (e IN ($2, $3)%4$s)
                    ), k AS (
                        SELECT me, a, mv, tx, bool_or(added) AS st, count(*) AS n
                          FROM m GROUP BY me, a, mv, tx
                    ), p AS (
                        SELECT k.*, lag(st) OVER (PARTITION BY me, a, mv ORDER BY tx) AS prev
                          FROM k
                    ), r AS (
                        SELECT m.e, m.a, m.v, m.tx, p.st = coalesce(p.prev, false) AS noop,
                               row_number() OVER (PARTITION BY m.me, m.a, m.mv, m.tx
                                                  ORDER BY m.added = p.st DESC) AS rn
                          FROM m JOIN p USING (me, a, mv, tx)
                         WHERE p.n > 1
                    )
                    DELETE FROM %1$I.%2$I d USING r
                     WHERE d.store_id = $1 AND d.e = r.e AND d.a = r.a AND d.v = r.v
                       AND d.tx = r.tx AND (r.noop OR r.rn > 1)$q$,
                    s, 'datoms_' || ty || '_new', vmap, vhit)
                    USING sid, rec.moved_to, rec.owner;
                EXECUTE format('UPDATE %1$I.%2$I SET e = CASE WHEN e = $2 THEN $3 ELSE e END, v = %3$s
                                 WHERE store_id = $1 AND (e = $2%4$s)',
                               s, 'datoms_' || ty || '_new', vmap,
                               CASE WHEN ty = 'ref' THEN ' OR v = $2' ELSE '' END)
                    USING sid, rec.moved_to, rec.owner;
                EXECUTE format('UPDATE %I.%I SET e = $1 WHERE store_id = $2 AND e = $3',
                               s, 'current_' || ty)
                    USING rec.owner, sid, rec.moved_to;
            END LOOP;
            EXECUTE format('UPDATE %I.current_ref SET v = $1 WHERE store_id = $2 AND v = $3', s)
                USING rec.owner, sid, rec.moved_to;
            repaired := true;
            action := 'repaired';
          EXCEPTION WHEN OTHERS THEN
            action := 'failed: ' || SQLERRM;
          END;
        END IF;
        RETURN NEXT;
    END LOOP;
    IF repaired THEN
        UPDATE mentat.cache_generation SET gen = gen + 1 WHERE store_name = store;
    END IF;
END;
$fn$;
