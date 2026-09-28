-- pg_mentat regression: basic queries
-- Point lookups, multi-join, aggregate, predicate

-- Point lookup: find name where age = 30
SELECT edn_q(
  '[:find ?name :where [?e :person/name ?name] [?e :person/age 30]]',
  '{}'::jsonb
);

-- Multi-join: name + age pairs (ordered)
SELECT edn_q(
  '[:find ?name ?age :where [?e :person/name ?name] [?e :person/age ?age] :order (asc ?name)]',
  '{}'::jsonb
);

-- Aggregate: count all persons
SELECT edn_q(
  '[:find (count ?e) :where [?e :person/name _]]',
  '{}'::jsonb
);

-- Predicate: age >= 30
SELECT edn_q(
  '[:find ?name ?age :where [?e :person/name ?name] [?e :person/age ?age] [(>= ?age 30)] :order (asc ?name)]',
  '{}'::jsonb
);

-- Input binding: parameterized lookup
SELECT edn_q(
  '[:find ?name :in ?target-age :where [?e :person/name ?name] [?e :person/age ?target-age]]',
  '{"inputs": [25]}'::jsonb
);
