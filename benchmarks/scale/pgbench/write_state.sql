-- write_mixed / sustained writer: set a random issue's state, or add a label.
\set i random(0, :n_issues - 1)
\set k random(1, 5)
\set l random(0, :n_labels - 1)
\set w random(0, 1)
SELECT edn_t(CASE WHEN :w::int = 0
  THEN '[[:db/add ' || (:i0::bigint + :i::bigint) || ' :issue/state ' || (ARRAY[':state/open',':state/in-progress',':state/closed',':state/resolved',':state/reopened'])[:k::int] || ']]'
  ELSE '[[:db/add ' || (:i0::bigint + :i::bigint) || ' :issue/label ' || (:l0::bigint + :l::bigint) || ']]' END);
