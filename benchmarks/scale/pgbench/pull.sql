-- edn_pull [*] on a random issue.
\set i random(0, :n_issues - 1)
SELECT edn_pull('[*]', :i0::bigint + :i::bigint);
