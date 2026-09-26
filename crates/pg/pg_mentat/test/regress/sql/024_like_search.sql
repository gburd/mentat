-- pg_mentat regression: LIKE and ILIKE predicate support
-- Pattern matching on string attributes

-- Setup: test data with varied text
\echo Setup: schema for LIKE/ILIKE tests

SELECT edn_t('[
  {:db/ident :lk/title
   :db/valueType :db.type/string
   :db/cardinality :db.cardinality/one}
]');

SELECT edn_t('[
  {:lk/title "PostgreSQL Extension"}
  {:lk/title "MySQL Connector"}
  {:lk/title "postgresql driver"}
  {:lk/title "Redis Cache"}
  {:lk/title "SQL Optimizer"}
]');

-- Test: LIKE with % wildcard (case-sensitive)
\echo Test: like pattern matching

SELECT edn_q(
  '[:find ?t :where [?e :lk/title ?t] [(like ?t "%SQL%")] :order (asc ?t)]',
  '{}'::jsonb
);

-- Test: ILIKE for case-insensitive matching
\echo Test: ilike case-insensitive matching

SELECT edn_q(
  '[:find ?t :where [?e :lk/title ?t] [(ilike ?t "%sql%")] :order (asc ?t)]',
  '{}'::jsonb
);

-- Test: LIKE with prefix pattern
\echo Test: like prefix matching

SELECT edn_q(
  '[:find ?t :where [?e :lk/title ?t] [(like ?t "Post%")] :order (asc ?t)]',
  '{}'::jsonb
);

-- Test: LIKE with no matches
\echo Test: like with no matches returns empty

SELECT edn_q(
  '[:find ?t :where [?e :lk/title ?t] [(like ?t "%ZZZZZ%")]]',
  '{}'::jsonb
);
