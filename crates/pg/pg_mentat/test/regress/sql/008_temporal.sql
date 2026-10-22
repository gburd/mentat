-- pg_mentat regression: temporal queries (as-of, since, history)

-- as-of the newest transaction: the same as current state
SELECT edn_q(
  '[:find (count ?e) :where [?e :person/name _]]',
  pg_catalog.jsonb_build_object('asOf', (SELECT max(tx) FROM mentat.transactions))
);

-- Basic query without temporal (should return same as current)
SELECT edn_q(
  '[:find (count ?e) :where [?e :person/name _]]',
  '{}'::jsonb
);
