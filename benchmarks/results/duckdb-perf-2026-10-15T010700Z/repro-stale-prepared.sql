-- DuckDB 1.5.6: a prepared statement with a constant filter is planned against
-- the column statistics at PREPARE time. If the constant is outside them, the
-- plan reads no rows, and re-executing it never sees rows inserted later.
-- A bound parameter (a = ?) is not affected here (joins can be: #24182).
-- Known upstream: duckdb/duckdb#23704 / #21207, fixed by PR #24696 (main,
-- 2026-08-14; in no 1.5.x release). Run: duckdb < this file.
CREATE TABLE t (a BIGINT);
INSERT INTO t SELECT range FROM range(1, 6);
PREPARE p AS SELECT count(*) AS cached FROM t WHERE a = 10;
PREPARE q AS SELECT count(*) AS cached_param FROM t WHERE a = $1;
EXECUTE p;            -- 0 (right)
INSERT INTO t VALUES (10);
EXECUTE p;            -- 0 (WRONG: one row has a = 10)
EXECUTE q(10);        -- 1
SELECT count(*) AS fresh FROM t WHERE a = 10;  -- 1
