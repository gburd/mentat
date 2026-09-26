-- pg_mentat regression: excision (permanent deletion)
-- Tests for mentat_excise() which physically removes entities

-- Setup: define schema and insert test entities
\echo Setup: schema for excision tests

SELECT edn_t('[
  {:db/ident :exc/name
   :db/valueType :db.type/string
   :db/cardinality :db.cardinality/one}
  {:db/ident :exc/ref
   :db/valueType :db.type/ref
   :db/cardinality :db.cardinality/one}
]');

SELECT edn_t('[
  {:exc/name "target-entity"}
  {:exc/name "other-entity"}
]');

-- Enable excision on the user partition
UPDATE mentat.partitions SET allow_excision = true
  WHERE part = ':db.part/user';

-- Find the entity id of "target-entity"
SELECT edn_q(
  '[:find ?e :where [?e :exc/name "target-entity"]]',
  '{}'::jsonb
);

-- Test: excise an entity and verify it disappears from queries
\echo Test: excise entity removes it from queries

-- Excise using the entity id (we use a subquery pattern)
DO $$
DECLARE
  eid bigint;
BEGIN
  SELECT (edn_q(
    '[:find ?e . :where [?e :exc/name "target-entity"]]',
    '{}'::jsonb
  ))::jsonb->>0 INTO eid;

  IF eid IS NOT NULL THEN
    PERFORM mentat_excise('default', ARRAY[eid]::bigint[], 'test excision');
  END IF;
END $$;

-- Verify entity is gone
SELECT edn_q(
  '[:find ?e :where [?e :exc/name "target-entity"]]',
  '{}'::jsonb
);

-- Other entity should still exist
SELECT edn_q(
  '[:find ?name :where [?e :exc/name ?name]]',
  '{}'::jsonb
);

-- Test: excise schema entity fails
\echo Test: excise schema entity denied

-- Entity 1 is :db/ident (a schema entity), should fail
SELECT mentat_excise('default', ARRAY[1]::bigint[], 'should fail');

-- Test: excise with dangling references fails
\echo Test: excise with dangling refs denied

SELECT edn_t('[
  {:exc/name "parent"}
]');

-- Create a reference to parent
DO $$
DECLARE
  parent_eid bigint;
BEGIN
  SELECT (edn_q(
    '[:find ?e . :where [?e :exc/name "parent"]]',
    '{}'::jsonb
  ))::jsonb->>0 INTO parent_eid;

  IF parent_eid IS NOT NULL THEN
    -- Add a reference from another entity to parent
    EXECUTE format(
      'SELECT edn_t(''[{:exc/name "child" :exc/ref %s}]'')',
      parent_eid
    );
    -- Now try to excise parent (should fail due to dangling ref)
    BEGIN
      PERFORM mentat_excise('default', ARRAY[parent_eid]::bigint[], 'should fail');
    EXCEPTION WHEN OTHERS THEN
      RAISE NOTICE 'Expected error: %', SQLERRM;
    END;
  END IF;
END $$;
