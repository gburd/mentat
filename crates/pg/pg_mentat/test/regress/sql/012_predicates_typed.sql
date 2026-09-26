-- pg_mentat regression: type-aware predicates
-- Verify that predicates work correctly on text, numeric, and mixed types.
-- This tests the fix for the NUMERIC coercion bug where text predicates like
-- [(= ?name "Bob")] would fail with "operator does not exist: numeric = text".

-- Test 1: Text equality predicate (the original bug)
SELECT edn_q(
  '[:find ?name :where [?e :person/name ?name] [(= ?name "Alice")]]',
  '{}'::jsonb
);

-- Test 2: Text inequality
SELECT edn_q(
  '[:find ?name :where [?e :person/name ?name] [(!= ?name "Alice")] :order (asc ?name)]',
  '{}'::jsonb
);

-- Test 3: Numeric predicate still works (regression guard)
SELECT edn_q(
  '[:find ?name ?age :where [?e :person/name ?name] [?e :person/age ?age] [(>= ?age 30)] :order (asc ?name)]',
  '{}'::jsonb
);

-- Test 4: Text predicate inside NOT clause
SELECT edn_q(
  '[:find ?name :where [?e :person/name ?name] (not [?e :person/name ?n] [(= ?n "Alice")]) :order (asc ?name)]',
  '{}'::jsonb
);
