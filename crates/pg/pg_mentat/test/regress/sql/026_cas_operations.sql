-- pg_mentat regression: compare-and-swap (:db.fn/cas)
-- Atomic conditional updates

-- Setup: entity to CAS on
\echo Setup: schema for CAS tests

SELECT edn_t('[
  {:db/ident :cas/counter
   :db/valueType :db.type/long
   :db/cardinality :db.cardinality/one}
  {:db/ident :cas/label
   :db/valueType :db.type/string
   :db/cardinality :db.cardinality/one}
]');

SELECT edn_t('[
  {:cas/label "test-entity" :cas/counter 100}
]');

-- Verify initial state
SELECT edn_q(
  '[:find ?v :where [?e :cas/label "test-entity"] [?e :cas/counter ?v]]',
  '{}'::jsonb
);

-- Test: successful CAS (old value matches)
\echo Test: CAS succeeds when old value matches

SELECT edn_t('[
  [:db.fn/cas [:cas/label "test-entity"] :cas/counter 100 200]
]');

-- Counter should now be 200
SELECT edn_q(
  '[:find ?v :where [?e :cas/label "test-entity"] [?e :cas/counter ?v]]',
  '{}'::jsonb
);

-- Test: failed CAS (old value does NOT match)
\echo Test: CAS fails when old value mismatches

-- Try to swap 999 -> 300, but current value is 200 (should error)
DO $$
BEGIN
  PERFORM edn_t('[
    [:db.fn/cas [:cas/label "test-entity"] :cas/counter 999 300]
  ]');
EXCEPTION WHEN OTHERS THEN
  RAISE NOTICE 'CAS failed as expected: %', SQLERRM;
END $$;

-- Value should still be 200 (unchanged after failed CAS)
SELECT edn_q(
  '[:find ?v :where [?e :cas/label "test-entity"] [?e :cas/counter ?v]]',
  '{}'::jsonb
);

-- Test: CAS with nil old-value (assert attribute not set)
\echo Test: CAS with nil asserts no current value

SELECT edn_t('[
  {:db/ident :cas/optional
   :db/valueType :db.type/long
   :db/cardinality :db.cardinality/one}
]');

-- CAS nil -> 42 should succeed (attribute not set on this entity)
SELECT edn_t('[
  [:db.fn/cas [:cas/label "test-entity"] :cas/optional nil 42]
]');

SELECT edn_q(
  '[:find ?v :where [?e :cas/label "test-entity"] [?e :cas/optional ?v]]',
  '{}'::jsonb
);
