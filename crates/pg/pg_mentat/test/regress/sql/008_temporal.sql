-- pg_mentat regression: temporal queries (as-of, since, history)

-- as-of: query current state (latest tx)
SELECT edn_q(
  '[:find (count ?e) :where [?e :person/name _]]',
  '{"as_of": "now"}'::jsonb
);

-- Basic query without temporal (should return same as current)
SELECT edn_q(
  '[:find (count ?e) :where [?e :person/name _]]',
  '{}'::jsonb
);
