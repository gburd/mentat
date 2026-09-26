-- Test concurrent transaction correctness
-- Phase C: Lock-free tx_id allocation via sequences

-- Verify basic transaction allocation still works
SELECT edn_t('[
  [:db/add "t1" :db/ident :test/concurrent-a]
  [:db/add "t1" :db/valueType :db.type/string]
  [:db/add "t1" :db/cardinality :db.cardinality/one]
]');

-- Verify sequential transactions get increasing tx_ids
SELECT edn_t('[[:db/add "e1" :test/concurrent-a "value1"]]');
SELECT edn_t('[[:db/add "e2" :test/concurrent-a "value2"]]');
SELECT edn_t('[[:db/add "e3" :test/concurrent-a "value3"]]');

-- Verify all 3 values were stored correctly
SELECT edn_q(
  '[:find ?v :where [?e :test/concurrent-a ?v]]',
  '{}'
);

-- Verify transaction IDs are monotonically increasing
SELECT edn_q(
  '[:find ?tx :where [?e :test/concurrent-a ?v ?tx]]',
  '{}'
);
