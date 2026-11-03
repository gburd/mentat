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
-- holding its live [O :db/ident ident] datom: the shape the bug left. For a
-- bootstrap ident (:db/doc), O is its fixed entid (its schema row, below
-- 100): a store has no :db/ident datoms for those, so re-asserting one
-- ({:db/ident :db/doc :db/doc "..."}) moved the row to a new entity that also
-- got the :db/ident datom.
--   ident     the ident, ':color/red'
--   moved_to  N, the entid the idents row names
--   owner     O, the entity with the live :db/ident datom (or the bootstrap
--             entid)
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
    -- 10 is :db/ident; its values are stored without the leading ':'. A
    -- bootstrap ident's entity is its schema row's (entid < 100).
    FOR rec IN EXECUTE format(
        'SELECT i.ident, i.entid AS moved_to, coalesce(b.entid, k.e) AS owner
           FROM %1$I.idents i
           LEFT JOIN %1$I.schema b ON b.ident = i.ident AND b.entid < 100
           LEFT JOIN %1$I.current_keyword k
             ON k.store_id = $1 AND k.a = 10 AND k.v = substr(i.ident, 2)
          WHERE coalesce(b.entid, k.e) <> i.entid
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
-- mismatch, two values of a cardinality-one attribute, or one :db.unique
-- value on two entities fails the ident.
--
-- A row with a blocker is skipped and left as it is; an ident whose repair
-- fails is rolled back on its own. Returns one row per ident: action is what
-- was done ('repaired', 'would repair', 'skipped: why', or 'failed: error'),
-- and notes (NULL if none, one per line) what the merge changes besides
-- moving N's datoms onto O, read from the log before the rewrite, so a dry
-- run shows them too:
--   past overlap: from tx T1 until tx T2, O has two values of a
--     cardinality-one attribute (one written through each entity), or two
--     entities both hold O as one :db.unique value. As of those
--     transactions, queries show both.
--   [e a v] is live now and retracted after the repair: a later retraction
--     through the other entity wins (or the current row has no datom in the
--     log at all). The reverse, not live now and live after, only shows on
--     a store whose current state already disagreed with its log.
-- At most 50 lines per ident, then a count of the rest.
--
-- A real run blocks every edn_t call on the store until the transaction it
-- runs in ends: it takes the store's write key exclusive, which each call
-- takes shared before it resolves an ident, so a writer waits, then reloads
-- the idents and writes to O (under READ COMMITTED; a REPEATABLE READ or
-- SERIALIZABLE writer still sees the idents of its first snapshot). It
-- waits for calls in flight to commit first. Back up first; run it in your
-- own transaction and check moved_idents(store) before COMMIT.
-- ---------------------------------------------------------------------------
CREATE OR REPLACE FUNCTION mentat.repair_moved_idents(
    store   TEXT    DEFAULT 'default',
    dry_run BOOLEAN DEFAULT true
)
RETURNS TABLE (ident TEXT, moved_to BIGINT, owner BIGINT, action TEXT, notes TEXT)
LANGUAGE plpgsql
SET search_path = pg_catalog, pg_temp
AS $fn$
DECLARE
    s TEXT;
    sid BIGINT;
    rec RECORD;
    nrow RECORD;
    ty TEXT;
    vmap TEXT;
    moved TEXT;
    scope TEXT;
    bad TEXT;
    ent TEXT;
    attr TEXT;
    shown TEXT;
    nk INT[];
    nt BIGINT[];
    nl TEXT[];
    h BIGINT;
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
        -- A snapshot taken before the wait below would miss what the writers it
        -- waited for wrote, and leave their refs on the empty entity.
        IF pg_catalog.current_setting('transaction_isolation') <> 'read committed' THEN
            RAISE EXCEPTION 'pg_mentat: repair_moved_idents(%, dry_run => false) must run under READ COMMITTED, not %',
                store, pg_catalog.current_setting('transaction_isolation');
        END IF;
        -- Wait for, and hold off, every edn_t call on the store: each takes the
        -- store's advisory key shared before it resolves an ident (write_locks.rs:
        -- namespace "Mnt0", then FNV-1a-32 of the store_id's 8 little-endian
        -- bytes). Taken before the generation row, which a schema-changing call
        -- locks after the key, so the two can't wait for each other.
        h := 2166136261;
        FOR i IN 0..7 LOOP
            h := ((h # ((sid >> (8 * i)) & 255)) * 16777619) & 4294967295;
        END LOOP;
        PERFORM pg_advisory_xact_lock(1299084336,
            (CASE WHEN h >= 2147483648 THEN h - 4294967296 ELSE h END)::int4);
        -- Then make every backend reload its idents (and wait for a
        -- schema-changing transaction in flight, which holds this row).
        UPDATE mentat.cache_generation SET gen = gen + 1 WHERE store_name = store;
    END IF;
    -- How a note shows entity @ ($3 is O, $4 its ident) and attribute @.
    ent := format('coalesce(CASE WHEN @ = $3 THEN $4 END, '
                  '(SELECT ii.ident FROM %I.idents ii WHERE ii.entid = @), (@)::text)', s);
    attr := format('coalesce((SELECT sa.ident FROM %I.schema sa WHERE sa.entid = @), (@)::text)', s);
    FOR rec IN SELECT * FROM mentat.moved_idents(store) LOOP
        ident := rec.ident;
        moved_to := rec.moved_to;
        owner := rec.owner;
        notes := NULL;
        nk := '{}';
        nt := '{}';
        nl := '{}';
        IF rec.blocker IS NOT NULL THEN
            action := 'skipped: ' || rec.blocker;
        ELSE
          BEGIN
            IF NOT dry_run THEN
                EXECUTE format('UPDATE %I.idents SET entid = $1 WHERE ident = $2', s)
                    USING rec.owner, rec.ident;
            END IF;
            FOREACH ty IN ARRAY types LOOP
                -- $2 is N, $3 is O.
                IF ty = 'ref' THEN
                    vmap := 'CASE WHEN v = $2 THEN $3 ELSE v END';
                    moved := 'e = $2 OR v = $2';
                    scope := 'e IN ($2, $3) OR v IN ($2, $3)';
                    shown := ent;
                ELSE
                    vmap := 'v';
                    moved := 'e = $2';
                    scope := 'e IN ($2, $3)';
                    shown := CASE ty WHEN 'keyword' THEN $s$(':' || @)$s$
                                     WHEN 'text' THEN 'to_json(@)::text'
                                     ELSE '(@)::text' END;
                END IF;
                -- The notes. Each merged datom's state per transaction is
                -- asserted if any of its rows there is (the rule below); from
                -- its changes of state come the intervals it is live in. Only
                -- datoms with a row through N change: a pair of datoms both
                -- written only through O, or both only through N, overlapped
                -- before the repair too.
                FOR nrow IN EXECUTE format($q$
                    WITH k AS (
                        SELECT CASE WHEN e = $2 THEN $3 ELSE e END AS me, a, %3$s AS mv, tx,
                               bool_or(added) AS st, bool_or(%5$s) AS hit, bool_and(%5$s) AS allm
                          FROM %1$I.%2$I
                         WHERE store_id = $1 AND (%4$s)
                         GROUP BY 1, 2, 3, 4
                    ), kr AS (
                        -- Only these can overlap: O's own cardinality-one
                        -- attributes, and :db.unique refs to O.
                        SELECT * FROM (
                            SELECT k.*, sc.cardinality = 'one' AND k.me = $3 AS c1,
                                   sc.unique_constraint IS NOT NULL AND %12$s AS u1
                              FROM k JOIN %1$I.schema sc ON sc.entid = k.a) x
                         WHERE c1 OR u1
                    ), ch AS (
                        SELECT * FROM (
                            SELECT me, a, mv, tx, st, c1, u1,
                                   lag(st) OVER (PARTITION BY me, a, mv ORDER BY tx) AS prev,
                                   bool_or(hit) OVER (PARTITION BY me, a, mv) AS h,
                                   bool_and(allm) OVER (PARTITION BY me, a, mv) AS am
                              FROM kr) x
                         WHERE st IS DISTINCT FROM coalesce(prev, false)
                    ), live AS (
                        SELECT * FROM (
                            SELECT ch.*, lead(tx) OVER (PARTITION BY me, a, mv ORDER BY tx) AS t1
                              FROM ch) x
                         WHERE st
                    ), one AS (
                        SELECT * FROM live WHERE c1
                    ), uniq AS (
                        SELECT * FROM live WHERE u1
                    ), fin AS (
                        SELECT DISTINCT ON (me, a, mv) me, a, mv, st
                          FROM k ORDER BY me, a, mv, tx DESC
                    ), cur AS (
                        SELECT DISTINCT CASE WHEN e = $2 THEN $3 ELSE e END AS me, a, %3$s AS mv
                          FROM %1$I.%6$I
                         WHERE store_id = $1 AND (%4$s)
                    )
                    SELECT 1 AS k, greatest(i.tx, j.tx) AS t,
                           format('past overlap: from tx %%s until %%s, %%s has two values of %%s: %%s and %%s',
                                  greatest(i.tx, j.tx), coalesce('tx ' || least(i.t1, j.t1), 'now'),
                                  %7$s, %8$s, %9$s, %10$s) AS line
                      FROM one i
                      JOIN one j ON j.me = i.me AND j.a = i.a AND j.mv > i.mv
                     WHERE greatest(i.tx, j.tx) < coalesce(least(i.t1, j.t1), 9223372036854775807)
                       AND NOT ((NOT i.h AND NOT j.h) OR (i.am AND j.am))
                    UNION ALL
                    SELECT 1, greatest(i.tx, j.tx),
                           format('past overlap: from tx %%s until %%s, entities %%s and %%s both hold %%s %%s',
                                  greatest(i.tx, j.tx), coalesce('tx ' || least(i.t1, j.t1), 'now'),
                                  %7$s, %11$s, %8$s, $4)
                      FROM uniq i
                      JOIN uniq j ON j.a = i.a AND j.mv = i.mv AND j.me > i.me
                     WHERE greatest(i.tx, j.tx) < coalesce(least(i.t1, j.t1), 9223372036854775807)
                       AND NOT ((NOT i.h AND NOT j.h) OR (i.am AND j.am))
                    UNION ALL
                    SELECT 2, 0, format('[%%s %%s %%s] is live now and retracted after the repair (%%s)',
                                        %13$s, %14$s, %15$s,
                                        CASE WHEN fin.st IS NULL THEN 'it has no datom in the log'
                                             ELSE 'a later retraction wins' END)
                      FROM cur LEFT JOIN fin USING (me, a, mv)
                     WHERE NOT coalesce(fin.st, false)
                    UNION ALL
                    SELECT 2, 0, format('[%%s %%s %%s] is not live now and live after the repair',
                                        %16$s, %17$s, %18$s)
                      FROM fin
                     WHERE fin.st AND NOT EXISTS (SELECT 1 FROM cur
                                                   WHERE (cur.me, cur.a, cur.mv) = (fin.me, fin.a, fin.mv))$q$,
                    s, 'datoms_' || ty || '_new', vmap, scope, moved, 'current_' || ty,
                    replace(ent, '@', 'i.me'), replace(attr, '@', 'i.a'),
                    replace(shown, '@', 'i.mv'), replace(shown, '@', 'j.mv'),
                    replace(ent, '@', 'j.me'),
                    CASE WHEN ty = 'ref' THEN 'k.mv = $3' ELSE 'false' END,
                    replace(ent, '@', 'cur.me'), replace(attr, '@', 'cur.a'),
                    replace(shown, '@', 'cur.mv'),
                    replace(ent, '@', 'fin.me'), replace(attr, '@', 'fin.a'),
                    replace(shown, '@', 'fin.mv'))
                    USING sid, rec.moved_to, rec.owner, rec.ident
                LOOP
                    nk := nk || nrow.k;
                    nt := nt || nrow.t;
                    nl := nl || nrow.line;
                END LOOP;
                CONTINUE WHEN dry_run;
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
            IF dry_run THEN
                action := 'would repair';
            ELSE
                repaired := true;
                action := 'repaired';
            END IF;
          EXCEPTION WHEN OTHERS THEN
            action := 'failed: ' || SQLERRM;
          END;
        END IF;
        SELECT string_agg(u.l, E'\n' ORDER BY u.k, u.t, u.l) INTO notes
          FROM (SELECT * FROM unnest(nk, nt, nl) AS x(k, t, l) ORDER BY 1, 2, 3 LIMIT 50) u;
        IF cardinality(nl) > 50 THEN
            notes := notes || format(E'\n... and %s more', cardinality(nl) - 50);
        END IF;
        RETURN NEXT;
    END LOOP;
    IF repaired THEN
        UPDATE mentat.cache_generation SET gen = gen + 1 WHERE store_name = store;
    END IF;
END;
$fn$;
