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
            -- A cardinality-one attribute with a different value on each:
            -- the merged entity would hold two. (Two sets of a
            -- cardinality-many attribute merge into one.)
            EXECUTE format(
                'SELECT min(x.a) FROM %1$I.%2$I x
                   JOIN %1$I.schema sc ON sc.entid = x.a AND sc.cardinality = ''one''
                  WHERE x.store_id = $1 AND x.e = $2
                    AND EXISTS (SELECT 1 FROM %1$I.%2$I y
                                 WHERE y.store_id = $1 AND y.e = $3 AND y.a = x.a
                                   AND %3$s IS DISTINCT FROM %4$s)',
                s, 'current_' || ty,
                CASE WHEN ty = 'ref' THEN 'CASE WHEN y.v = $2 THEN $3 ELSE y.v END' ELSE 'y.v' END,
                CASE WHEN ty = 'ref' THEN 'CASE WHEN x.v = $2 THEN $3 ELSE x.v END' ELSE 'x.v' END)
                INTO hit USING sid, rec.moved_to, rec.owner;
            IF hit IS NOT NULL AND blocker IS NULL THEN
                blocker := format('both entities have a different value of attribute %s', hit);
            END IF;
            -- N as an attribute: its datoms would need N's schema row.
            EXECUTE format('SELECT count(*) FROM %I.%I WHERE store_id = $1 AND a = $2',
                           s, 'datoms_' || ty || '_new')
                INTO hit USING sid, rec.moved_to;
            IF hit > 0 AND blocker IS NULL THEN
                blocker := format('entity %s is used as an attribute', rec.moved_to);
            END IF;
        END LOOP;
        -- A :db.unique ref attribute that one entity has with value O and
        -- another with value N: both would hold one unique value.
        IF blocker IS NULL THEN
            EXECUTE format(
                'SELECT format(''entities %%s and %%s would both hold %%s %%s'', y.e, x.e, sc.ident, $4)
                   FROM %1$I.current_ref x
                   JOIN %1$I.schema sc ON sc.entid = x.a AND sc.unique_constraint IS NOT NULL
                   JOIN %1$I.current_ref y ON y.store_id = $1 AND y.a = x.a AND y.v = $3
                  WHERE x.store_id = $1 AND x.v = $2
                    AND CASE WHEN x.e = $2 THEN $3 ELSE x.e END
                        <> CASE WHEN y.e = $2 THEN $3 ELSE y.e END
                  ORDER BY sc.ident, y.e, x.e LIMIT 1', s)
                INTO blocker USING sid, rec.moved_to, rec.owner, rec.ident;
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
-- back at O, and move what was written to N onto O in the log: datoms with
-- N as entity get O, ref values that point at N point at O. Transaction ids
-- stay as they are, so the history keeps when each value was written; as-of
-- a time after the move now shows the value on O.
--
-- A datom written through both entities becomes one datom with one merged
-- history. A row that only repeats the state the datom already had is
-- dropped (no as-of answer changes): re-loading {:c/name "car" :c/color
-- :color/red} after the move, in one transaction, retracted [car :c/color O]
-- and asserted [car :c/color N]; the repair keeps neither row, and car's
-- color stays O, asserted when it was first written. The latest row wins,
-- as for any datom: a value asserted through O and retracted through N is
-- retracted. The current state of the merged datoms is then recomputed from
-- the log, so it equals as-of the latest transaction, and checked: a
-- mismatch, or two values of a cardinality-one attribute, fails the ident.
--
-- A row with a blocker is skipped and left as it is; an ident whose repair
-- fails is rolled back on its own. Returns one row per ident with what was
-- done ('repaired', 'would repair', 'skipped: why', or 'failed: error').
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
    moved TEXT;
    scope TEXT;
    bad TEXT;
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
                -- $2 is N, $3 is O.
                IF ty = 'ref' THEN
                    vmap := 'CASE WHEN v = $2 THEN $3 ELSE v END';
                    moved := 'e = $2 OR v = $2';
                    scope := 'e IN ($2, $3) OR v IN ($2, $3)';
                ELSE
                    vmap := 'v';
                    moved := 'e = $2';
                    scope := 'e IN ($2, $3)';
                END IF;
                -- In each datom that has a row through N, drop the rows that
                -- repeat its state, and keep one row per transaction (its net
                -- effect: asserted if any row of it is).
                EXECUTE format($q$
                    WITH m AS (
                        SELECT e, a, v, tx, added, (%4$s) AS moved,
                               CASE WHEN e = $2 THEN $3 ELSE e END AS me, %3$s AS mv
                          FROM %1$I.%2$I
                         WHERE store_id = $1 AND (%5$s)
                    ), k AS (
                        SELECT me, a, mv, tx, bool_or(added) AS st, bool_or(moved) AS hit
                          FROM m GROUP BY me, a, mv, tx
                    ), p AS (
                        SELECT k.*, lag(st) OVER w AS prev, bool_or(hit) OVER (PARTITION BY me, a, mv) AS mixed
                          FROM k WINDOW w AS (PARTITION BY me, a, mv ORDER BY tx)
                    ), r AS (
                        SELECT m.e, m.a, m.v, m.tx, p.st = coalesce(p.prev, false) AS noop,
                               row_number() OVER (PARTITION BY m.me, m.a, m.mv, m.tx
                                                  ORDER BY m.added = p.st DESC) AS rn
                          FROM m JOIN p USING (me, a, mv, tx)
                         WHERE p.mixed
                    )
                    DELETE FROM %1$I.%2$I d USING r
                     WHERE d.store_id = $1 AND d.e = r.e AND d.a = r.a AND d.v = r.v
                       AND d.tx = r.tx AND (r.noop OR r.rn > 1)$q$,
                    s, 'datoms_' || ty || '_new', vmap, moved, scope)
                    USING sid, rec.moved_to, rec.owner;
                EXECUTE format('UPDATE %1$I.%2$I SET e = CASE WHEN e = $2 THEN $3 ELSE e END, v = %3$s
                                 WHERE store_id = $1 AND (%4$s)',
                               s, 'datoms_' || ty || '_new', vmap, moved)
                    USING sid, rec.moved_to, rec.owner;
                -- The current state of those datoms, from the log: the latest
                -- row of each wins (mentat.rebuild_current_projection's rule).
                EXECUTE format('DELETE FROM %1$I.%2$I WHERE store_id = $1 AND (%3$s)',
                               s, 'current_' || ty, scope)
                    USING sid, rec.moved_to, rec.owner;
                EXECUTE format($q$
                    INSERT INTO %1$I.%2$I (store_id, e, a, v, tx)
                    SELECT store_id, e, a, v, tx
                      FROM (SELECT DISTINCT ON (e, a, v) store_id, e, a, v, tx, added
                              FROM %1$I.%3$I
                             WHERE store_id = $1 AND (%4$s)
                             ORDER BY e, a, v, tx DESC) l
                     WHERE added$q$,
                    s, 'current_' || ty, 'datoms_' || ty || '_new', scope)
                    USING sid, rec.moved_to, rec.owner;
                -- Check what the repair wrote: current == log, one value per
                -- cardinality-one attribute, one entity per :db.unique value.
                EXECUTE format($q$
                    WITH l AS (
                        SELECT e, a, v FROM (SELECT DISTINCT ON (e, a, v) e, a, v, added
                                               FROM %1$I.%3$I
                                              WHERE store_id = $1 AND (%4$s)
                                              ORDER BY e, a, v, tx DESC) x
                         WHERE added
                    ), c AS (
                        SELECT e, a, v FROM %1$I.%2$I WHERE store_id = $1 AND (%4$s)
                    )
                    SELECT coalesce(
                        (SELECT format('the current state of entity %%s differs from its log', e)
                           FROM ((SELECT * FROM l EXCEPT SELECT * FROM c)
                                 UNION ALL (SELECT * FROM c EXCEPT SELECT * FROM l)) d LIMIT 1),
                        (SELECT format('entity %%s has two values of attribute %%s', c.e, c.a)
                           FROM c JOIN %1$I.schema sc ON sc.entid = c.a AND sc.cardinality = 'one'
                          GROUP BY c.e, c.a HAVING count(*) > 1 LIMIT 1),
                        (SELECT format('two entities hold one value of unique attribute %%s', c.a)
                           FROM c JOIN %1$I.schema sc ON sc.entid = c.a
                                 AND sc.unique_constraint IS NOT NULL
                          GROUP BY c.a, c.v HAVING count(*) > 1 LIMIT 1))$q$,
                    s, 'current_' || ty, 'datoms_' || ty || '_new', scope)
                    INTO bad USING sid, rec.moved_to, rec.owner;
                IF bad IS NOT NULL THEN
                    RAISE EXCEPTION '%', bad;
                END IF;
            END LOOP;
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
