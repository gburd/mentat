(require "tests/test")
(require '[mino.store :as store])

;; mino.store — EAVT fact store.
;;
;; Each store holds an appending log of facts [e a v tx instant op] and a
;; materialized entity view. These tests cover lifecycle, transactions
;; (add/retract/map sugar), point/temporal/recency reads, aggregation,
;; durability, stats, and compaction.

(def store-test-dir "/tmp/mino-store-test")

;; ---------------------------------------------------------------------------
;; Lifecycle
;; ---------------------------------------------------------------------------

(deftest store-open-in-memory
  (let [conn (store/open)]
    (is (store? conn))
    (is (map? (store/db conn)))
    (store/close conn)))

(deftest store-db-and-deref-agree
  (let [conn (store/open)]
    (is (= (store/db conn) @conn))
    (store/close conn)))

(deftest store-empty-db-shape
  (let [db (store/db (store/open))]
    (is (= {} (:entities db)))
    (is (= [] (:log db)))
    (is (= 0 (:tx db)))))

(deftest store-predicate
  (is (store? (store/open)))
  (is (not (store? 42)))
  (is (not (store? "not a store")))
  (is (not (store? nil)))
  (is (not (store? {:entities {}}))))

(deftest store-close-idempotent
  (let [conn (store/open)]
    (store/close conn)
    (is (nil? (store/close conn)) "second close is a no-op")))

(deftest store-in-memory-checkpoint-noop
  (let [conn (store/open)]
    (is (nil? (store/checkpoint conn)) "in-memory checkpoint does not throw")
    (store/close conn)))

;; ---------------------------------------------------------------------------
;; Transact — add
;; ---------------------------------------------------------------------------

(deftest store-transact-add-single
  (let [conn (store/open)
        r (store/transact conn [:db/add 1 :name "Alice"])
        db (store/db conn)]
    (is (= 0 (:tx r)) "first applied tx is 0")
    (is (= "Alice" (store/read db 1 :name)))
    (is (= 1 (:tx db)) "db advances to 1")))

(deftest store-transact-return-shape
  (let [conn (store/open)
        r (store/transact conn [:db/add 1 :a 1])]
    (is (contains? r :tx))
    (is (contains? r :db-after))
    (is (= (store/db conn) (:db-after r)))))

(deftest store-transact-map-sugar
  (let [conn (store/open)
        _ (store/transact conn {1 {:name "Alice" :age 30}})
        db (store/db conn)]
    (is (= "Alice" (store/read db 1 :name)))
    (is (= 30 (store/read db 1 :age)))
    (is (= 1 (count (store/entities db))))))

(deftest store-transact-multiple-facts
  (let [conn (store/open)
        _ (store/transact conn [[:db/add 1 :name "Alice"]
                                [:db/add 2 :name "Bob"]
                                [:db/add 3 :name "Carol"]])
        db (store/db conn)]
    (is (= #{1 2 3} (store/entities db)))
    (is (= 1 (:tx db)) "one transact advances tx by one")
    (is (= 3 (count (:log db))) "three facts in the log")))

(deftest store-tx-increments-per-transact
  (let [conn (store/open)
        r1 (store/transact conn [:db/add 1 :a 1])
        r2 (store/transact conn [:db/add 1 :a 2])
        r3 (store/transact conn [:db/add 1 :a 3])
        db (store/db conn)]
    (is (= 0 (:tx r1)))
    (is (= 1 (:tx r2)))
    (is (= 2 (:tx r3)))
    (is (= 3 (:tx db)))))

(deftest store-facts-appear-in-log
  (let [conn (store/open)
        _ (store/transact conn [:db/add 1 :name "Alice"])
        _ (store/transact conn [:db/add 1 :age 30])
        log (:log (store/db conn))]
    (is (= 2 (count log)))
    (let [f0 (first log) f1 (second log)]
      (is (= :db/add (:op f0)))
      (is (= 1 (:e f0)))
      (is (= :name (:a f0)))
      (is (= 0 (:tx f0)))
      (is (= 1 (:tx f1))))))

(deftest store-put-asserts-single-fact
  (let [conn (store/open)
        _ (store/put conn 1 :name "Alice")
        db (store/db conn)]
    (is (= "Alice" (store/read db 1 :name)))))

;; ---------------------------------------------------------------------------
;; Transact — retract
;; ---------------------------------------------------------------------------

(deftest store-retract-attribute
  (let [conn (store/open)
        _ (store/transact conn {1 {:name "Alice" :age 30}})
        _ (store/transact conn [:db/retract 1 :name])
        db (store/db conn)]
    (is (nil? (store/read db 1 :name)))
    (is (= 30 (store/read db 1 :age)))
    (is (store/entity-exists? db 1) "other attributes keep the entity alive")))

(deftest store-retract-specific-value-matching
  (let [conn (store/open)
        _ (store/transact conn [:db/add 1 :name "Alice"])
        _ (store/transact conn [:db/retract 1 :name "Alice"])
        db (store/db conn)]
    (is (nil? (store/read db 1 :name)))))

(deftest store-retract-specific-value-nonmatching
  (let [conn (store/open)
        _ (store/transact conn [:db/add 1 :name "Alice"])
        _ (store/transact conn [:db/retract 1 :name "Bob"])
        db (store/db conn)]
    (is (= "Alice" (store/read db 1 :name)) "mismatched value is a no-op")))

(deftest store-retract-nonexistent-entity-noop
  (let [conn (store/open)
        _ (store/transact conn [:db/add 1 :name "Alice"])
        _ (store/transact conn [:db/retract 999 :name])
        db (store/db conn)]
    (is (= "Alice" (store/read db 1 :name)))
    (is (not (store/entity-exists? db 999)))))

(deftest store-retract-removes-entity-when-empty
  (let [conn (store/open)
        _ (store/transact conn [:db/add 1 :name "Alice"])
        _ (store/transact conn [:db/retract 1 :name])
        db (store/db conn)]
    (is (not (store/entity-exists? db 1)))
    (is (not (contains? (:entities db) 1)))
    (is (= #{} (store/entities db)))))

(deftest store-retract-fn-attribute
  (let [conn (store/open)
        _ (store/put conn 1 :name "Alice")
        _ (store/retract conn 1 :name)
        db (store/db conn)]
    (is (nil? (store/read db 1 :name)))))

(deftest store-retract-set-member
  ;; Cardinality: a set-valued attribute supports per-member retract.
  (let [conn (store/open)
        _ (store/transact conn [:db/add 1 :tags #{:a :b :c}])
        _ (store/transact conn [:db/retract 1 :tags :b])
        db (store/db conn)]
    (is (= #{:a :c} (store/read db 1 :tags)))))

(deftest store-retract-last-set-member-drops-attribute
  (let [conn (store/open)
        _ (store/transact conn {1 {:tags #{:only}}})
        _ (store/transact conn [:db/retract 1 :tags :only])
        db (store/db conn)]
    (is (nil? (store/read db 1 :tags)))
    (is (not (store/entity-exists? db 1)) "last attribute gone removes entity")))

;; ---------------------------------------------------------------------------
;; Transact -- malformed tx-data
;; ---------------------------------------------------------------------------

(deftest store-tx-data-add-missing-value-throws
  ;; [:db/add e a] (3-tuple) is missing v and must NOT be silently
  ;; rewritten as :db/retract.
  (let [conn (store/open)]
    (is (thrown? (store/transact conn [:db/add 1 :name])))
    (is (= #{} (store/entities (store/db conn))) "no facts applied on throw")
    (is (= 0 (:tx (store/db conn))) "tx counter not advanced on throw")))

(deftest store-tx-data-add-extra-value-throws
  ;; [:db/add e a v extra] (5-tuple) is malformed.
  (let [conn (store/open)]
    (is (thrown? (store/transact conn [:db/add 1 :name "X" :extra])))
    (is (= #{} (store/entities (store/db conn))))
    (is (= 0 (:tx (store/db conn))))))

(deftest store-tx-data-retract-extra-value-throws
  ;; [:db/retract e a v extra] (5-tuple) is malformed.
  (let [conn (store/open)]
    (is (thrown? (store/transact conn [:db/retract 1 :name "X" :extra])))
    (is (= 0 (:tx (store/db conn))))))

(deftest store-tx-data-retract-three-tuple-still-works
  ;; The 3-tuple [:db/retract e a] sugar for "retract whole attribute"
  ;; must keep working after the arity tightening.
  (let [conn (store/open)
        _ (store/transact conn [:db/add 1 :name "Alice"])
        _ (store/transact conn [:db/retract 1 :name])
        db (store/db conn)]
    (is (not (store/entity-exists? db 1)))))

;; ---------------------------------------------------------------------------
;; Point reads
;; ---------------------------------------------------------------------------

(deftest store-read-vector-form
  (let [conn (store/open)
        _ (store/transact conn [:db/add 1 :name "Alice"])
        db (store/db conn)]
    (is (= "Alice" (store/read db [1 :name])))
    (is (nil? (store/read db [1 :missing])))
    (is (nil? (store/read db [999 :name])))))

(deftest store-read-spread-form
  (let [conn (store/open)
        _ (store/transact conn [:db/add 1 :name "Alice"])
        db (store/db conn)]
    (is (= "Alice" (store/read db 1 :name)))
    (is (= "Alice" (store/read db [1 :name])) "both forms agree")))

(deftest store-entity-returns-map-with-id
  (let [conn (store/open)
        _ (store/transact conn {1 {:name "Alice" :age 30}})
        db (store/db conn)]
    (is (= {:db/id 1 :name "Alice" :age 30} (store/entity db 1)))
    (is (nil? (store/entity db 999)))))

(deftest store-entity-exists
  (let [conn (store/open)
        _ (store/transact conn [:db/add 1 :name "Alice"])
        db (store/db conn)]
    (is (store/entity-exists? db 1))
    (is (not (store/entity-exists? db 999)))))

;; ---------------------------------------------------------------------------
;; Empty-store edge cases
;; ---------------------------------------------------------------------------

(deftest store-empty-store-reads
  (let [db (store/db (store/open))]
    (is (= #{} (store/entities db)))
    (is (nil? (store/read db 1 :name)))
    (is (nil? (store/entity db 1)))
    (is (not (store/entity-exists? db 1)))
    (is (= 0 (count (store/history db 1))))
    (is (empty? (store/recent db 5)))))

(deftest store-add-nil-value
  ;; v1 is single-valued and nil is a storable value; the entity exists
  ;; even though reading the nil attribute looks like absence.
  (let [conn (store/open)
        _ (store/transact conn [:db/add 1 :note nil])
        db (store/db conn)]
    (is (store/entity-exists? db 1) "entity with a nil attribute still exists")
    (is (nil? (store/read db 1 :note)))
    (is (= {:db/id 1 :note nil} (store/entity db 1)))))

;; ---------------------------------------------------------------------------
;; Temporal reads
;; ---------------------------------------------------------------------------

(deftest store-as-of-by-tx
  (let [conn (store/open)
        _ (store/transact conn [:db/add 1 :a :v0])   ;; tx 0
        _ (store/transact conn [:db/add 1 :a :v1])   ;; tx 1
        _ (store/transact conn [:db/add 1 :a :v2])   ;; tx 2
        db (store/db conn)]                          ;; :tx 3
    (is (nil? (store/read (store/as-of db 0) 1 :a)) "before any tx")
    (is (= :v0 (store/read (store/as-of db 1) 1 :a)) "state after tx 0")
    (is (= :v1 (store/read (store/as-of db 2) 1 :a)) "state after tx 1")
    (is (= :v2 (store/read (store/as-of db 3) 1 :a)) "current state")))

(deftest store-since-by-tx
  (let [conn (store/open)
        _ (store/transact conn [:db/add 1 :a :v0])   ;; tx 0
        _ (store/transact conn [:db/add 1 :a :v1])   ;; tx 1
        _ (store/transact conn [:db/add 1 :a :v2])   ;; tx 2
        db (store/db conn)]
    (is (= 3 (count (store/since db 0)) "all facts at or after tx 0"))
    (is (= 2 (count (store/since db 1)) "tx 1 and 2"))
    (is (= 1 (count (store/since db 2)) "only tx 2"))
    (is (empty? (store/since db 3)) "nothing at or after current tx")))

(deftest store-as-of-by-inst
  ;; Datomic parity: (as-of db inst) interprets the arg as a wall-clock
  ;; instant by TYPE, not by magnitude. Passing #inst must always take
  ;; the instant branch, regardless of how small the epoch-ms value is.
  ;; A typical break with the old magnitude heuristic: a fresh process
  ;; whose monotonic clock was < 1e9 had every instant misread as a tx.
  (let [conn (store/open)
        _ (store/transact conn [:db/add 1 :a :v0])   ;; tx 0
        _ (store/transact conn [:db/add 1 :a :v1])   ;; tx 1
        _ (store/transact conn [:db/add 1 :a :v2])   ;; tx 2
        db (store/db conn)]
    ;; A past inst (well before this test process started) sees no facts.
    (is (empty? (:entities (store/as-of db #inst "2020-01-01T00:00:00.000Z")))
        "past inst excludes all facts")
    ;; A far-future inst sees the current state.
    (is (= :v2 (store/read (store/as-of db #inst "2099-01-01T00:00:00.000Z") 1 :a))
        "future inst sees current state")))

(deftest store-since-by-inst
  ;; Symmetric: (since db inst) returns facts at or after the wall-clock
  ;; instant when the arg is an inst.
  (let [conn (store/open)
        _ (store/transact conn [:db/add 1 :a :v0])
        _ (store/transact conn [:db/add 1 :a :v1])
        db (store/db conn)]
    (is (= 2 (count (store/since db #inst "2020-01-01T00:00:00.000Z"))
           "past inst includes all facts"))
    (is (empty? (store/since db #inst "2099-01-01T00:00:00.000Z"))
        "future inst excludes all facts")))

(deftest store-instants-are-wall-clock-ms
  ;; ADR 11 documents :instant as "wall-clock milliseconds". Recorded
  ;; :instant values must be in the same epoch-ms space as #inst and
  ;; inst-ms so that (as-of db #inst ...) and the recorded :instants
  ;; actually compare against each other usefully. (Pre-fix the clock
  ;; returned monotonic-ms, which only coincidentally ordered facts
  ;; within one process and never aligned with #inst.)
  (let [conn (store/open)
        before (inst-ms #inst "2024-01-01T00:00:00.000Z")
        _ (store/transact conn [:db/add 1 :a :v0])
        db (store/db conn)
        recorded (:instant (first (:log db)))]
    (is (> recorded before)
        "recorded :instant is in epoch-ms space (greater than a 2024 inst)")))

(deftest store-history-entity
  (let [conn (store/open)
        _ (store/transact conn [:db/add 1 :name "Alice"])    ;; tx 0
        _ (store/transact conn [:db/add 1 :name "Alicia"])   ;; tx 1
        _ (store/transact conn [:db/add 2 :name "Bob"])      ;; tx 2
        db (store/db conn)]
    (is (= 2 (count (store/history db 1))) "two facts for entity 1")
    (is (= 1 (count (store/history db 2))) "one fact for entity 2")
    (is (empty? (store/history db 999)) "no history for absent entity")
    (is (= ["Alice" "Alicia"] (map :v (store/history db 1))) "log order")))

(deftest store-history-entity-attribute
  (let [conn (store/open)
        _ (store/transact conn [:db/add 1 :name "Alice"])
        _ (store/transact conn [:db/add 1 :age 30])
        _ (store/transact conn [:db/add 1 :name "Alicia"])
        db (store/db conn)]
    (is (= 2 (count (store/history db 1 :name))))
    (is (= 1 (count (store/history db 1 :age))))))

;; ---------------------------------------------------------------------------
;; Recency
;; ---------------------------------------------------------------------------

(deftest store-recent-most-recent-first
  (let [conn (store/open)
        _ (store/transact conn [:db/add 1 :a 1])
        _ (store/transact conn [:db/add 2 :a 2])
        _ (store/transact conn [:db/add 3 :a 3])
        db (store/db conn)]
    (is (= [3 2 1] (map :e (store/recent db 10))) "most recent first")
    (is (= [3 2] (map :e (store/recent db 2))) "bounded to n")
    (is (empty? (store/recent db 0)) "zero returns nothing")))

(deftest store-recent-where-filter
  (let [conn (store/open)
        _ (store/transact conn [[:db/add 1 :a 1]
                                [:db/retract 1 :a]
                                [:db/add 2 :a 2]])
        db (store/db conn)]
    (is (= [:db/retract]
           (map :op (store/recent db 10 {:where #(= (:op %) :db/retract)}))))))

(deftest store-recent-entities-scope
  (let [conn (store/open)
        _ (store/transact conn [[:db/add 1 :a 1]
                                [:db/add 2 :a 2]
                                [:db/add 1 :a 3]])
        db (store/db conn)]
    (is (= #{1} (set (map :e (store/recent db 10 {:entities #{1}})))))))

(deftest store-recent-attributes-scope
  (let [conn (store/open)
        _ (store/transact conn [[:db/add 1 :x 1]
                                [:db/add 1 :y 2]
                                [:db/add 1 :x 3]])
        db (store/db conn)]
    (is (= #{:x} (set (map :a (store/recent db 10 {:attributes #{:x}})))))))

;; ---------------------------------------------------------------------------
;; Collection reads
;; ---------------------------------------------------------------------------

(deftest store-entities-set
  (let [conn (store/open)
        _ (store/transact conn [[:db/add 1 :a 1] [:db/add 2 :a 2]])
        db (store/db conn)]
    (is (= #{1 2} (store/entities db)))
    (is (= #{} (store/entities (store/db (store/open)))))))

(deftest store-where-filter
  (let [conn (store/open)
        _ (store/transact conn {1 {:role :admin} 2 {:role :user} 3 {:role :admin}})
        db (store/db conn)]
    (let [admins (store/where db #(= (:role %) :admin))]
      (is (= 2 (count admins)))
      (is (= #{1 3} (set (map :db/id admins)))))))

(deftest store-find-by-single
  (let [conn (store/open)
        _ (store/transact conn [:db/add 1 :email "a@x.com"])
        db (store/db conn)]
    (is (= 1 (store/find-by db :email "a@x.com")))))

(deftest store-find-by-multiple
  (let [conn (store/open)
        _ (store/transact conn [[:db/add 1 :role :admin] [:db/add 2 :role :admin]])
        db (store/db conn)]
    (is (= #{1 2} (store/find-by db :role :admin)))))

(deftest store-find-by-none
  (let [conn (store/open)
        _ (store/transact conn [:db/add 1 :role :admin])
        db (store/db conn)]
    (is (nil? (store/find-by db :role :user)))))

;; ---------------------------------------------------------------------------
;; Projection and aggregation
;; ---------------------------------------------------------------------------

(deftest store-project-fn
  (let [conn (store/open)
        _ (store/transact conn [[:db/add 1 :a 1] [:db/add 2 :a 2]])
        db (store/db conn)]
    (is (= 2 (store/project db #(count (store/entities %)))))))

(deftest store-project-map
  (let [conn (store/open)
        _ (store/transact conn {1 {:name "Alice" :age 30}})
        db (store/db conn)]
    (is (= {:name "Alice" :age 30}
           (store/project db {:name [1 :name] :age [1 :age]})))))

(deftest store-project-map-missing-path
  (let [conn (store/open)
        _ (store/transact conn [:db/add 1 :name "Alice"])
        db (store/db conn)]
    (is (= {:name "Alice" :age nil}
           (store/project db {:name [1 :name] :age [1 :age]})))))

(deftest store-merge-disjoint-entities
  (let [conn-a (store/open)
        conn-b (store/open)
        _ (store/transact conn-a [:db/add 1 :a 1])
        _ (store/transact conn-b [:db/add 2 :b 2])
        db-a (store/db conn-a)
        db-b (store/db conn-b)
        merged (store/merge db-a db-b)]
    (is (= #{1 2} (store/entities merged)))
    (is (= (+ (count (:log db-a)) (count (:log db-b)))
           (count (:log merged))))))

(deftest store-merge-last-write-wins
  ;; Literal db values with controlled :instant make the merge order
  ;; deterministic; the later instant wins on conflicting writes.
  (let [db-a {:entities {1 {:v 1}}
              :log [{:e 1 :a :v :v 1 :tx 0 :instant 100 :op :db/add}]
              :tx 1}
        db-b {:entities {1 {:v 2}}
              :log [{:e 1 :a :v :v 2 :tx 0 :instant 200 :op :db/add}]
              :tx 1}
        merged (store/merge db-a db-b)]
    (is (= 2 (store/read merged 1 :v)) "later instant wins")
    (is (= 1 (:tx merged)) "tx is the max of the two")))

(deftest store-fold-default-extract
  ;; fold's default extract-fn is entity count.
  (let [db-a {:entities {1 {} 2 {}} :log [] :tx 0}
        db-b {:entities {3 {}}       :log [] :tx 0}]
    (is (= 3 (store/fold [db-a db-b] + 0)))))

(deftest store-fold-custom-extract
  (let [db-a {:entities {} :log [{:tx 0} {:tx 1}] :tx 2}
        db-b {:entities {} :log [{:tx 0}]          :tx 1}]
    (is (= 3 (store/fold [db-a db-b] + 0 (fn [db] (count (:log db))))))))

(deftest store-concat-logs
  (let [db-a {:entities {} :log [{:e 1 :instant 300}] :tx 1}
        db-b {:entities {} :log [{:e 2 :instant 100}] :tx 1}
        db-c {:entities {} :log [{:e 3 :instant 200}] :tx 1}]
    (is (= [100 200 300] (map :instant (store/concat-logs db-a db-b db-c))))))

;; ---------------------------------------------------------------------------
;; Pure variant
;; ---------------------------------------------------------------------------

(deftest store-with-pure-variant
  ;; store/with applies tx-data to a db value without a connection, so it
  ;; must derive an instant without a store (the store-clock* nil path).
  (let [conn (store/open)
        _ (store/transact conn [:db/add 1 :a 1])
        db (store/db conn)
        r (store/with db [:db/add 1 :a 2])
        db' (:db-after r)]
    (is (= 2 (store/read db' 1 :a)) "speculated value")
    (is (= 1 (store/read db 1 :a)) "original db unchanged")
    (is (= (:tx db) (:tx r)) ":tx returned is the applied tx")))

;; ---------------------------------------------------------------------------
;; Stats
;; ---------------------------------------------------------------------------

(deftest store-stats-empty
  (let [db (store/db (store/open))]
    (is (= {:facts 0 :entities 0 :tx 0} (store/stats db)))))

(deftest store-stats-populated
  (let [conn (store/open)
        _ (store/transact conn {1 {:a 1} 2 {:b 2}})
        db (store/db conn)]
    (is (= {:facts 2 :entities 2 :tx 1} (store/stats db)))))

;; ---------------------------------------------------------------------------
;; Compaction
;; ---------------------------------------------------------------------------

(deftest store-compact-clears-log
  (let [conn (store/open)
        _ (store/transact conn [:db/add 1 :a 1])
        _ (store/transact conn [:db/add 1 :a 2])
        _ (store/transact conn [:db/add 2 :b 3])
        _ (store/compact conn)
        db (store/db conn)]
    (is (empty? (:log db)) "log cleared")
    (is (= 2 (count (:entities db))) "view preserved")
    (is (= 2 (store/read db 1 :a)) "value preserved")
    (is (= 3 (:tx db)) "tx preserved")))

(deftest store-compact-keep-last
  (let [conn (store/open)
        _ (store/transact conn [:db/add 1 :a 1])
        _ (store/transact conn [:db/add 1 :a 2])
        _ (store/transact conn [:db/add 1 :a 3])
        _ (store/transact conn [:db/add 1 :a 4])
        _ (store/compact conn {:keep-last 2})
        db (store/db conn)]
    (is (= 2 (count (:log db))) "last 2 facts kept")
    (is (= [3 4] (map :v (:log db))) "kept the most recent")
    (is (= 4 (store/read db 1 :a)) "view intact")))

;; ---------------------------------------------------------------------------
;; Durability
;; ---------------------------------------------------------------------------

(deftest store-durable-survives-reopen
  (rm-rf store-test-dir)
  (mkdir-p store-test-dir)
  (let [path (str store-test-dir "/snap.db")]
    (try
      (let [conn (store/open path)
            _ (store/transact conn {1 {:name "Alice" :age 30}})
            _ (store/checkpoint conn)
            _ (store/close conn)
            conn2 (store/open path)
            db (store/db conn2)]
        (is (= "Alice" (store/read db 1 :name)))
        (is (= 30 (store/read db 1 :age)))
        (is (= #{1} (store/entities db)))
        (store/close conn2))
      (finally
        (rm-rf store-test-dir)))))

;; ---------------------------------------------------------------------------
;; WAL durability
;; ---------------------------------------------------------------------------

(deftest store-wal-survives-without-checkpoint
  ;; Transact without checkpoint, reopen: WAL replay recovers the data.
  (rm-rf store-test-dir)
  (mkdir-p store-test-dir)
  (let [path (str store-test-dir "/wal1.db")]
    (try
      (let [conn (store/open path)
            _ (store/transact conn {1 {:name "Alice"}})
            ;; Don't checkpoint or close — simulate crash by opening a new conn
            conn2 (store/open path)
            db (store/db conn2)]
        (is (= "Alice" (store/read db 1 :name)) "WAL replay recovers data"))
      (finally
        (rm-rf store-test-dir)))))

(deftest store-wal-multiple-transactions
  ;; Multiple transactions between checkpoints are all replayed.
  (rm-rf store-test-dir)
  (mkdir-p store-test-dir)
  (let [path (str store-test-dir "/wal2.db")]
    (try
      (let [conn (store/open path)
            _ (store/transact conn [:db/add 1 :name "Alice"])
            _ (store/transact conn [:db/add 2 :name "Bob"])
            _ (store/transact conn [:db/add 3 :name "Carol"])
            conn2 (store/open path)
            db (store/db conn2)]
        (is (= #{"Alice" "Bob" "Carol"}
               (set (map #(store/read db % :name) [1 2 3]))))
        (is (= 3 (:tx db)) "tx counter preserved across WAL replay"))
      (finally
        (rm-rf store-test-dir)))))

(deftest store-wal-checkpoint-deletes-wal
  ;; After checkpoint, the WAL file is deleted.
  (rm-rf store-test-dir)
  (mkdir-p store-test-dir)
  (let [path (str store-test-dir "/wal3.db")
        wal-path (str path ".wal")]
    (try
      (let [conn (store/open path)
            _ (store/transact conn [:db/add 1 :name "Alice"])
            _ (store/checkpoint conn)]
        (is (not (file-exists? wal-path)) "WAL deleted after checkpoint"))
      (finally
        (rm-rf store-test-dir)))))

(deftest store-wal-torn-write-recovery
  ;; A malformed trailing WAL line is silently dropped.
  (rm-rf store-test-dir)
  (mkdir-p store-test-dir)
  (let [path (str store-test-dir "/wal4.db")
        wal-path (str path ".wal")]
    (try
      (let [conn (store/open path)
            _ (store/transact conn [:db/add 1 :name "Alice"])
            ;; Append garbage to simulate a torn write
            _ (spit wal-path "GARBAGE{not valid" :append true)
            conn2 (store/open path)
            db (store/db conn2)]
        (is (= "Alice" (store/read db 1 :name)) "good entry recovered")
        (is (= 1 (:tx db)) "tx counter from good entry"))
      (finally
        (rm-rf store-test-dir)))))

(deftest store-wal-checkpoint-then-transact
  ;; Snapshot captures state, new WAL entries are replayed on top.
  (rm-rf store-test-dir)
  (mkdir-p store-test-dir)
  (let [path (str store-test-dir "/wal5.db")]
    (try
      (let [conn (store/open path)
            _ (store/transact conn [:db/add 1 :name "Alice"])
            _ (store/checkpoint conn)
            _ (store/transact conn [:db/add 2 :name "Bob"])
            conn2 (store/open path)
            db (store/db conn2)]
        (is (= "Alice" (store/read db 1 :name)) "from snapshot")
        (is (= "Bob" (store/read db 2 :name)) "from WAL replay"))
      (finally
        (rm-rf store-test-dir)))))

(deftest store-wal-stale-entries-skipped
  (rm-rf store-test-dir)
  (mkdir-p store-test-dir)
  (let [path (str store-test-dir "/stale.db")
        wal-path (str path ".wal")]
    (try
      (let [conn (store/open path)
            _ (store/transact conn [:db/add 1 :name "Alice"])
            stale-line (slurp wal-path)
            _ (store/transact conn [:db/add 1 :age 30])
            _ (store/checkpoint conn)
            conn2 (store/open path)
            _ (store/transact conn2 [:db/add 2 :name "Carol"])
            fresh-line (slurp wal-path)
            _ (spit wal-path (str stale-line fresh-line))
            conn3 (store/open path)
            db (store/db conn3)
            name-facts (filter #(and (= (:e %) 1) (= (:a %) :name))
                               (:log db))]
        (is (= "Alice" (store/read db 1 :name)) "snapshot value present")
        (is (= 30 (store/read db 1 :age)) "snapshot value present")
        (is (= "Carol" (store/read db 2 :name)) "fresh WAL entry replayed")
        (is (= 1 (count name-facts)) "stale entry not double-applied to log")
        (is (= 3 (:tx db)) "tx advanced past the fresh entry only")
        (store/close conn3))
      (finally
        (rm-rf store-test-dir)))))

(deftest store-snapshot-atomic-write
  ;; Checkpoint writes to <path>.tmp then renames into place, so a
  ;; stale .tmp from a crashed previous attempt is cleaned up and the
  ;; canonical snapshot is never left half-written.
  (rm-rf store-test-dir)
  (mkdir-p store-test-dir)
  (let [path (str store-test-dir "/snap_atomic.db")
        tmp-path (str path ".tmp")]
    (try
      (let [conn (store/open path)
            _ (store/transact conn [:db/add 1 :name "Alice"])
            _ (store/checkpoint conn)
            ;; Simulate a stale .tmp left by a crashed checkpoint attempt
            _ (spit tmp-path "STALE GARBAGE FROM CRASHED CHECKPOINT")
            conn2 (store/open path)
            _ (store/transact conn2 [:db/add 2 :name "Bob"])
            _ (store/checkpoint conn2)
            conn3 (store/open path)
            db (store/db conn3)]
        (is (= "Alice" (store/read db 1 :name)) "first snapshot intact")
        (is (= "Bob" (store/read db 2 :name)) "second checkpoint applied")
        (is (not (file-exists? tmp-path))
            "stale .tmp cleaned up by atomic rename")
        (store/close conn3))
      (finally
        (rm-rf store-test-dir)))))

;; ---------------------------------------------------------------------------
;; Schema validation
;; ---------------------------------------------------------------------------

(deftest store-schema-accepts-valid-type
  (let [conn (store/open nil {:schema {:name {:type :string}}})
        _ (store/transact conn [:db/add 1 :name "Alice"])
        db (store/db conn)]
    (is (= "Alice" (store/read db 1 :name)))))

(deftest store-schema-rejects-wrong-type
  (let [conn (store/open nil {:schema {:age {:type :long}}})]
    (is (thrown? (store/transact conn [:db/add 1 :age "not a number"])))))

(deftest store-schema-many-cardinality
  (let [conn (store/open nil {:schema {:tags {:cardinality :many}}})
        _ (store/transact conn [:db/add 1 :tags :a])
        _ (store/transact conn [:db/add 1 :tags :b])
        _ (store/transact conn [:db/add 1 :tags :c])
        db (store/db conn)]
    (is (= #{:a :b :c} (store/read db 1 :tags)))))

(deftest store-schema-many-retract-member
  (let [conn (store/open nil {:schema {:tags {:cardinality :many}}})
        _ (store/transact conn [:db/add 1 :tags :a])
        _ (store/transact conn [:db/add 1 :tags :b])
        _ (store/transact conn [:db/retract 1 :tags :a])
        db (store/db conn)]
    (is (= #{:b} (store/read db 1 :tags)))))

(deftest store-schema-closed-rejects-unknown
  (let [conn (store/open nil {:schema {:name {:type :string}}
                              :closed true})]
    (is (thrown? (store/transact conn [:db/add 1 :unknown "value"])))))

(deftest store-schema-closed-accepts-declared
  (let [conn (store/open nil {:schema {:name {:type :string}}
                              :closed true})
        _ (store/transact conn [:db/add 1 :name "Alice"])
        db (store/db conn)]
    (is (= "Alice" (store/read db 1 :name)))))

(deftest store-schema-open-allows-unknown
  ;; Default (open schema) allows any attribute.
  (let [conn (store/open nil {:schema {:name {:type :string}}})
        _ (store/transact conn [:db/add 1 :name "Alice"])
        _ (store/transact conn [:db/add 1 :extra "anything"])
        db (store/db conn)]
    (is (= "Alice" (store/read db 1 :name)))
    (is (= "anything" (store/read db 1 :extra)))))

(deftest store-schema-returns-schema
  (let [schema {:name {:type :string} :tags {:cardinality :many}}
        conn (store/open nil {:schema schema})
        db (store/db conn)]
    (is (= schema (store/schema db)))))

(deftest store-schema-keyword-type
  (let [conn (store/open nil {:schema {:role {:type :keyword}}})
        _ (store/transact conn [:db/add 1 :role :admin])
        db (store/db conn)]
    (is (= :admin (store/read db 1 :role)))))

(deftest store-schema-rejects-unknown-type-spec
  ;; A typo in the type keyword (e.g. :strng) must not silently disable
  ;; validation. The schema declares :str, but :str is not a supported
  ;; type. The schema should be rejected at transact time so the typo
  ;; surfaces rather than silently accepting every value.
  (let [conn (store/open nil {:schema {:name {:type :strng}}})]
    (is (thrown? (store/transact conn [:db/add 1 :name "Alice"])))
    (is (thrown? (store/transact conn [:db/add 1 :name 42])))))

(deftest store-schema-long-rejects-overflowing-bigint
  ;; :long means 64-bit signed; bigints beyond Long/MAX_VALUE must be
  ;; rejected, not silently accepted as integers.
  (let [conn (store/open nil {:schema {:n {:type :long}}})]
    (is (thrown? (store/transact conn [:db/add 1 :n 9223372036854775808N])))
    (let [r (store/transact conn [:db/add 1 :n 1])]
      (is (= 1 (store/read (:db-after r) 1 :n))
          "in-range long still accepted"))))

(deftest store-schema-many-single-overwrite-without-schema
  ;; Without schema :many, :db/add overwrites (single-valued).
  (let [conn (store/open)
        _ (store/transact conn [:db/add 1 :tags :a])
        _ (store/transact conn [:db/add 1 :tags :b])
        db (store/db conn)]
    (is (= :b (store/read db 1 :tags)) "overwrites without :many")))

(deftest store-tx-data-rejects-nil-entity-id
  ;; nil as an entity id collides with "absent" in point reads and lets
  ;; callers create a phantom nil-keyed entity.
  (let [conn (store/open)]
    (is (thrown? (store/transact conn [:db/add nil :name "X"])))
    (is (= #{} (store/entities (store/db conn))))
    (is (= 0 (:tx (store/db conn))))))

(deftest store-tx-data-rejects-nil-attribute
  ;; nil as an attribute is meaningless and indistinguishable from
  ;; "absent" on read.
  (let [conn (store/open)]
    (is (thrown? (store/transact conn [:db/add 1 nil "X"])))
    (is (= #{} (store/entities (store/db conn))))
    (is (= 0 (:tx (store/db conn))))))

(deftest store-tx-data-rejects-degenerate-attribute
  ;; An attribute key names a property of an entity. An empty string or
  ;; empty keyword is always a caller bug -- a typo, an unbound slot, or
  ;; a missing input. Reject at validate-fact rather than storing under
  ;; a key that no later read can usefully reference.
  (let [conn (store/open)]
    (is (thrown? (store/transact conn [:db/add 1 "" "X"]))
        "empty string attribute rejected")
    (is (thrown? (store/transact conn [:db/add 1 (keyword "") "X"]))
        "empty keyword attribute rejected")
    (is (= #{} (store/entities (store/db conn))))
    (is (= 0 (:tx (store/db conn))))))

(deftest store-tx-data-rejects-degenerate-eid
  ;; Entity ids must be positive integers (Datomic reserves negatives
  ;; for tempids; mino has no tempids yet, so negatives are reserved)
  ;; or keywords (natural-key style; mino also accepts lookup-refs
  ;; resolved before validate-fact). Zero, floats, strings, and
  ;; unresolved vectors are caller bugs and must throw.
  (let [conn (store/open)]
    (is (thrown? (store/transact conn [:db/add -1 :name "Neg"]))
        "negative eid rejected")
    (is (thrown? (store/transact conn [:db/add 0 :name "Zero"]))
        "zero eid rejected")
    (is (thrown? (store/transact conn [:db/add 1.5 :name "Float"]))
        "float eid rejected")
    (is (thrown? (store/transact conn [:db/add "alice" :name "Str"]))
        "string eid rejected")
    (is (thrown? (store/transact conn [:db/add [:email "x@y.z"] :name "Lookup"]))
        "unresolved lookup-ref eid rejected")
    ;; Positive ints and keywords remain valid.
    (is (store/transact conn [:db/add 1 :name "Alice"]))
    (is (store/transact conn [:db/add :bob :name "Bob"]))
    (is (= #{1 :bob} (store/entities (store/db conn))))))

;; ---------------------------------------------------------------------------
;; Indexes
;; ---------------------------------------------------------------------------

(deftest store-index-find-by-single
  (let [conn (store/open nil {:indexes #{:email}})
        _ (store/transact conn [:db/add 1 :email "a@x.com"])
        db (store/db conn)]
    (is (= 1 (store/find-by db :email "a@x.com")))))

(deftest store-index-find-by-multiple
  (let [conn (store/open nil {:indexes #{:role}})
        _ (store/transact conn [[:db/add 1 :role :admin]
                                [:db/add 2 :role :admin]])
        db (store/db conn)]
    (is (= #{1 2} (store/find-by db :role :admin)))))

(deftest store-index-find-by-none
  (let [conn (store/open nil {:indexes #{:email}})
        _ (store/transact conn [:db/add 1 :email "a@x.com"])
        db (store/db conn)]
    (is (nil? (store/find-by db :email "missing@x.com")))))

(deftest store-index-maintained-on-update
  ;; When a value changes, the old index entry is removed.
  (let [conn (store/open nil {:indexes #{:email}})
        _ (store/transact conn [:db/add 1 :email "old@x.com"])
        _ (store/transact conn [:db/add 1 :email "new@x.com"])
        db (store/db conn)]
    (is (nil? (store/find-by db :email "old@x.com")) "old value gone from index")
    (is (= 1 (store/find-by db :email "new@x.com")) "new value indexed")))

(deftest store-index-maintained-on-retract
  (let [conn (store/open nil {:indexes #{:email}})
        _ (store/transact conn [:db/add 1 :email "a@x.com"])
        _ (store/transact conn [:db/retract 1 :email])
        db (store/db conn)]
    (is (nil? (store/find-by db :email "a@x.com")) "retracted value removed from index")))

(deftest store-index-fallback-scan
  ;; Non-indexed attributes fall back to linear scan.
  (let [conn (store/open)
        _ (store/transact conn [[:db/add 1 :name "Alice"]
                                [:db/add 2 :name "Bob"]])
        db (store/db conn)]
    (is (= 1 (store/find-by db :name "Alice")))
    (is (= 2 (store/find-by db :name "Bob")))))

(deftest store-index-many-cardinality
  ;; Indexes work with :many cardinality (find entities with a specific tag).
  (let [conn (store/open nil {:schema {:tags {:cardinality :many}}
                              :indexes #{:tags}})
        _ (store/transact conn [[:db/add 1 :tags :a]
                                [:db/add 1 :tags :b]
                                [:db/add 2 :tags :a]])
        db (store/db conn)]
    (is (= #{1 2} (store/find-by db :tags :a)) "both entities have :a")
    (is (= 1 (store/find-by db :tags :b)) "only entity 1 has :b")))

;; ---------------------------------------------------------------------------
;; Retention
;; ---------------------------------------------------------------------------

(deftest store-retention-keep-last
  ;; Auto-compaction triggers when log exceeds 2×keep-last.
  (let [conn (store/open nil {:history {:keep-last 3}})
        _ (store/transact conn [:db/add 1 :a 1])
        _ (store/transact conn [:db/add 1 :a 2])
        _ (store/transact conn [:db/add 1 :a 3])
        ;; At 3 facts, threshold is 6 — no compaction yet
        db3 (store/db conn)]
    (is (= 3 (count (:log db3))) "log not compacted below threshold")
    (let [_ (store/transact conn [:db/add 1 :a 4])
          _ (store/transact conn [:db/add 1 :a 5])
          _ (store/transact conn [:db/add 1 :a 6])
          _ (store/transact conn [:db/add 1 :a 7])
          db7 (store/db conn)]
      (is (<= (count (:log db7)) 6) "log compacted after exceeding 2×threshold")
      (is (= 7 (store/read db7 1 :a)) "current value preserved"))))

(deftest store-retention-keep-since
  ;; :keep-since drops facts older than the cutoff.
  (let [conn (store/open nil {:history {:keep-since 200}})
        _ (store/transact conn [:db/add 1 :a :old])  ;; instant ~100
        _ (store/transact conn [:db/add 2 :a :new])  ;; instant ~200+
        db (store/db conn)]
    ;; With keep-since 200, facts before 200 should be compacted when
    ;; the threshold is exceeded. We check entities are preserved.
    (is (= :new (store/read db 2 :a)) "entity 2 preserved")
    (is (= :old (store/read db 1 :a)) "entity 1 preserved in view")))

(deftest store-retention-preserves-tx
  (let [conn (store/open nil {:history {:keep-last 2}})
        _ (store/transact conn [:db/add 1 :a 1])
        _ (store/transact conn [:db/add 1 :a 2])
        _ (store/transact conn [:db/add 1 :a 3])
        _ (store/transact conn [:db/add 1 :a 4])
        _ (store/transact conn [:db/add 1 :a 5])
        db (store/db conn)]
    (is (= 5 (:tx db)) "tx counter preserved after compaction")))

;; ---------------------------------------------------------------------------
;; Datalog query
;; ---------------------------------------------------------------------------

(deftest store-q-basic-find
  (let [conn (store/open)
        _ (store/transact conn {1 {:name "Alice"} 2 {:name "Bob"}})
        db (store/db conn)]
    (is (= #{[1] [2]}
           (store/q db '[:find ?e :where [?e :name ?n]])))))

(deftest store-q-find-two-vars
  (let [conn (store/open)
        _ (store/transact conn {1 {:name "Alice"} 2 {:name "Bob"}})
        db (store/db conn)]
    (is (= #{[1 "Alice"] [2 "Bob"]}
           (store/q db '[:find ?e ?name :where [?e :name ?name]])))))

(deftest store-q-constant-value
  (let [conn (store/open)
        _ (store/transact conn [[:db/add 1 :name "Alice"]
                                [:db/add 2 :name "Bob"]])
        db (store/db conn)]
    (is (= #{[1]}
           (store/q db '[:find ?e :where [?e :name "Alice"]])))))

(deftest store-q-join-two-patterns
  (let [conn (store/open)
        _ (store/transact conn {1 {:name "Alice" :age 30}
                                2 {:name "Bob" :age 25}})
        db (store/db conn)]
    (is (= #{[1 "Alice" 30] [2 "Bob" 25]}
           (store/q db '[:find ?e ?name ?age
                         :where [?e :name ?name]
                                [?e :age ?age]])))))

(deftest store-q-predicate-filter
  (let [conn (store/open)
        _ (store/transact conn {1 {:name "Alice" :age 30}
                                2 {:name "Bob" :age 25}
                                3 {:name "Carol" :age 35}})
        db (store/db conn)]
    (is (= #{[1 "Alice"] [3 "Carol"]}
           (store/q db '[:find ?e ?name
                         :where [?e :name ?name]
                                [?e :age ?age]
                                [(> ?age 28)]])))))

(deftest store-q-predicate-eq
  (let [conn (store/open)
        _ (store/transact conn {1 {:role :admin} 2 {:role :user}})
        db (store/db conn)]
    (is (= #{[1]}
           (store/q db '[:find ?e
                         :where [?e :role ?r]
                                [(= ?r :admin)]])))))

(deftest store-q-empty-result
  (let [conn (store/open)
        _ (store/transact conn [:db/add 1 :name "Alice"])
        db (store/db conn)]
    (is (= #{}
           (store/q db '[:find ?e :where [?e :name "Nonexistent"]])))))

(deftest store-q-with-index
  ;; Datalog works with indexed attributes.
  (let [conn (store/open nil {:indexes #{:email}})
        _ (store/transact conn [[:db/add 1 :email "a@x.com"]
                                [:db/add 2 :email "b@x.com"]
                                [:db/add 1 :name "Alice"]])
        db (store/db conn)]
    (is (= #{[1 "Alice"]}
           (store/q db '[:find ?e ?name
                         :where [?e :name ?name]
                                [?e :email "a@x.com"]])))))

(deftest store-q-index-variable-value
  ;; Datalog on indexed attr with variable value uses the index.
  (let [conn (store/open nil {:indexes #{:email}})
        _ (store/transact conn [[:db/add 1 :email "a@x.com"]
                                [:db/add 2 :email "b@x.com"]
                                [:db/add 3 :email "c@x.com"]])
        db (store/db conn)]
    (is (= #{[1 "a@x.com"] [2 "b@x.com"] [3 "c@x.com"]}
           (store/q db '[:find ?e ?email
                         :where [?e :email ?email]])))))

(deftest store-q-index-constant-eid
  ;; Datalog on indexed attr with constant entity-id uses the index.
  (let [conn (store/open nil {:indexes #{:email}})
        _ (store/transact conn [[:db/add 1 :email "a@x.com"]
                                [:db/add 2 :email "b@x.com"]])
        db (store/db conn)]
    (is (= #{["a@x.com"]}
           (store/q db '[:find ?email
                         :where [1 :email ?email]])))))

(deftest store-q-index-join
  ;; Datalog joins work when one pattern uses an indexed attr.
  (let [conn (store/open nil {:indexes #{:email}})
        _ (store/transact conn {1 {:email "a@x.com" :name "Alice"}
                                2 {:email "b@x.com" :name "Bob"}})
        db (store/db conn)]
    (is (= #{["Alice"]}
           (store/q db '[:find ?name
                         :where [?e :email "a@x.com"]
                                [?e :name ?name]])))))

(deftest store-q-malformed-throws
  (let [conn (store/open)
        _ (store/transact conn [:db/add 1 :name "Alice"])
        db (store/db conn)]
    (is (thrown? (store/q db {:not :a-query})) "non-vector throws")
    (is (thrown? (store/q db 42)) "scalar throws")
    (is (thrown? (store/q db '[:where [?e :a ?v]])) "missing :find throws")
    (is (thrown? (store/q db nil)) "nil throws")))

(deftest store-q-unbound-find-var-throws
  ;; A var referenced in :find but never bound by any :where clause is a
  ;; query-author error. It must throw, not silently return a column of
  ;; nils.
  (let [conn (store/open)
        _ (store/transact conn [:db/add 1 :name "Alice"])
        db (store/db conn)]
    (is (thrown? (store/q db '[:find ?e ?unbound
                               :where [?e :name ?n]]))
        "unbound var in :find throws")))

(deftest store-q-order-by-unbound-var-throws
  ;; :order-by must reference a var bound by some :where clause. An
  ;; :order-by var that no clause binds is a query-author error --
  ;; previously it silently sorted by nil, masking the typo.
  (let [conn (store/open)
        _ (store/transact conn [:db/add 1 :name "Alice"])
        db (store/db conn)]
    (is (thrown? (store/q db '[:find ?n :order-by ?missing :where
                              [?e :name ?n]]))
        ":order-by var not bound by any clause throws")))

(deftest store-schema-many-type-checks-set-members
  ;; A :cardinality :many attribute accepts a set value at tx time and
  ;; checks each member against the declared type. Without per-member
  ;; iteration the type check rejected the set itself ("expected :keyword").
  (let [conn (store/open nil {:schema {:tags {:type :keyword :cardinality :many}}})]
    (is (store/transact conn [:db/add 1 :tags #{:a :b :c}])
        "set of matching values accepted")
    (is (= #{:a :b :c} (store/read @conn 1 :tags)))
    (is (thrown? (store/transact conn [:db/add 2 :tags #{:ok :bad 42}]))
        "set with one bad member rejected as a whole")
    (store/close conn)))

(deftest store-find-by-range-many-cardinality
  ;; find-by-range on a :many attribute (set values) returns entities
  ;; with at least one in-range member. Previously the (>= v lo) check
  ;; crashed against a set.
  (let [conn (store/open nil {:schema {:score {:type :long :cardinality :many}}})
        _    (store/transact conn [:db/add 1 :score #{10 20 30}])
        _    (store/transact conn [:db/add 2 :score #{20 30 40}])
        _    (store/transact conn [:db/add 3 :score #{100 200 300}])]
    ;; range [15 35] catches entities 1 (20,30) and 2 (20,30), not 3.
    (is (= [1 2] (store/find-by-range @conn :score 15 35)))
    (store/close conn)))

(deftest store-as-of-rejects-non-inst-non-integer-point
  ;; as-of/since dispatch the temporal axis by argument type: inst for
  ;; wall-clock, integer for tx-number. Anything else is a query-author
  ;; error -- previously the comparator crashed at (< tx "garbage") with
  ;; a misleading "MTY001: < expects numbers".
  (let [conn (store/open)
        _    (store/put conn 1 :name "Alice")
        db   @conn]
    (is (thrown? (store/as-of db "not-a-tx-or-inst")) "as-of rejects string")
    (is (thrown? (store/as-of db nil)) "as-of rejects nil")
    (is (thrown? (store/as-of db [:also :bad])) "as-of rejects vector")
    (is (thrown? (store/since db "not-a-tx-or-inst")) "since rejects string")))

(deftest store-compact-rejects-bogus-keep-spec
  ;; compact's keep-spec must be {:keep-last N} or {:keep-since T}. An
  ;; unrecognized spec is a caller error -- previously the cond fell
  ;; through to :else and silently kept everything, masking the typo.
  (let [conn (store/open)
        _    (store/put conn 1 :name "Alice")
        _    (store/put conn 2 :name "Bob")]
    (is (thrown? (store/compact conn {:bogus-option 42})) "bogus key throws")
    (is (thrown? (store/compact conn {:keep-last "not-a-number"})) "bad type throws")
    (is (thrown? (store/compact conn nil)) "nil throws")
    (is (thrown? (store/compact conn 42)) "scalar throws")))

(deftest store-q-malformed-pattern-throws
  ;; Pattern clauses must be 3-element vectors [e a v]. Wrong arities and
  ;; non-vector clauses must throw, not silently produce empty results.
  (let [conn (store/open)
        _ (store/transact conn [:db/add 1 :name "Alice"])
        db (store/db conn)]
    (is (thrown? (store/q db '[:find ?e :where [?e :name]]))
        "2-element pattern throws")
    (is (thrown? (store/q db '[:find ?e :where [?e]]))
        "1-element pattern throws")
    (is (thrown? (store/q db '[:find ?e :where [?e :n ?v ?w]]))
        "4-element pattern throws")
    (is (thrown? (store/q db '[:find ?e :where :not-a-clause]))
        "non-vector clause throws")
    (is (thrown? (store/q db '[:find ?e :where []]))
        "empty vector clause throws")))

(deftest store-q-predicate-rejects-variable-head
  ;; A variable in the predicate-function position would let an :in-bound
  ;; value control the head of an eval'd form, which is a code-injection
  ;; vector. Validation must reject it; the predicate head must be a
  ;; literal symbol resolvable in the calling namespace.
  (let [conn (store/open)
        _    (store/transact conn [:db/add 1 :name "Alice"])
        db   (store/db conn)]
    (is (thrown? (store/q db '[:find ?e :in ?p :where
                              [?e :name ?n]
                              [(?p ?n)]]
                         'eval))
        "variable predicate head throws")
    (is (thrown? (store/q db '[:find ?e :in ?p ?arg :where
                              [?e :name ?n]
                              [(?p ?arg)]]
                         'eval '(println "pwned")))
        "variable predicate head with bound args throws")))

(deftest store-q-predicate-args-passed-as-data
  ;; A constant-head predicate clause applies the resolved function to the
  ;; substituted arg VALUES. The arg must never be eval'd as code: an
  ;; :in-bound form like (undefined-fn) is data, not an expression to run.
  ;; If the implementation substituted-and-eval'd, this query would throw
  ;; resolving the undefined symbol; if it applies-the-fn-to-the-value,
  ;; identity returns the form (truthy) and the row matches.
  (let [conn (store/open)
        _    (store/transact conn [:db/add 1 :name "Alice"])
        db   (store/db conn)]
    (is (= #{[1]}
           (store/q db '[:find ?e :in ?v :where
                         [?e :name ?n]
                         [(identity ?v)]]
                    '(undefined-fn)))
        "predicate arg is passed as data, not eval'd")))

;; ---------------------------------------------------------------------------
;; Domain-unique attributes, upsert, lookup-refs
;;
;; ---------------------------------------------------------------------------

(deftest store-unique-attr-upsert-creates
  ;; First write of a :unique :identity attribute creates a new entity.
  (let [conn (store/open nil {:schema {:email {:unique :identity}}})
        _ (store/transact conn [:db/add 100 :email "a@x.com"])
        db (store/db conn)]
    (is (= "a@x.com" (store/read db 100 :email)))
    (is (= #{100} (store/entities db)))))

(deftest store-unique-attr-upsert-merges
  ;; A second write that carries an already-seen unique value on a
  ;; not-yet-existing eid is rewritten onto the existing entity
  ;; (upsert) instead of creating a new one.
  (let [conn (store/open nil {:schema {:email {:unique :identity}}})
        _ (store/transact conn [:db/add 100 :email "a@x.com"])
        _ (store/transact conn [:db/add 999 :email "a@x.com"])
        db (store/db conn)]
    (is (= #{100} (store/entities db)) "no new entity for duplicate unique value")
    (is (= "a@x.com" (store/read db 100 :email)))))

(deftest store-lookup-ref-resolves
  ;; A 2-element vector [:unique-attr value] is accepted as an entity-id
  ;; in tx-data and resolved at parse time via the unique index.
  (let [conn (store/open nil {:schema {:email {:unique :identity}}})
        _ (store/transact conn [:db/add 100 :email "a@x.com"])
        _ (store/transact conn [:db/add [:email "a@x.com"] :name "Alice"])
        db (store/db conn)]
    (is (= "Alice" (store/read db 100 :name)) "lookup-ref resolved to entity 100")
    (is (= #{100} (store/entities db)) "no phantom entity created")))

(deftest store-unique-conflict-throws
  ;; Two distinct new entities carrying the same unique value within a
  ;; single tx must throw, tagged ::unique-conflict, and apply nothing.
  (let [conn (store/open nil {:schema {:email {:unique :identity}}})
        e (try
            (store/transact conn [[:db/add 100 :email "a@x.com"]
                                  [:db/add 200 :email "a@x.com"]])
            nil
            (catch e e))]
    (is (some? e) "conflicting tx throws")
    (is (some? (re-find #"unique-conflict" (pr-str (ex-data e))))
        "ex-data carries ::unique-conflict tag")
    (is (= #{} (store/entities (store/db conn))) "no facts applied on throw")
    (is (= 0 (:tx (store/db conn))) "tx counter not advanced on throw")))

(deftest store-unique-requires-cardinality-one
  ;; Declaring :unique :identity on a :cardinality :many attribute is a
  ;; schema error detected at open time.
  (is (thrown? (store/open nil {:schema {:tags {:cardinality :many
                                                :unique :identity}}}))))

(deftest store-unique-attr-auto-indexed
  ;; A :unique attribute is implicitly indexed; find-by works without an
  ;; explicit :indexes entry.
  (let [conn (store/open nil {:schema {:email {:unique :identity}}})
        _ (store/transact conn [[:db/add 1 :email "a@x.com"]
                                [:db/add 2 :email "b@x.com"]])
        db (store/db conn)]
    (is (= 1 (store/find-by db :email "a@x.com")))
    (is (= 2 (store/find-by db :email "b@x.com")))
    (is (nil? (store/find-by db :email "missing@x.com")))))

;; ---------------------------------------------------------------------------
;; Reference type + reverse index
;; ---------------------------------------------------------------------------

(deftest store-ref-validates-existing-eid
  ;; A :ref attribute accepts any existing eid as its value.
  (let [conn (store/open nil {:schema {:child {:type :ref}}})
        _ (store/transact conn [:db/add 1 :name "Parent"])
        _ (store/transact conn [:db/add 2 :child 1])
        db (store/db conn)]
    (is (= 1 (store/read db 2 :child)))))

(deftest store-ref-dangling-throws
  ;; A :ref value that is not an existing eid at apply time throws,
  ;; tagged ::dangling-ref, and applies nothing.
  (let [conn (store/open nil {:schema {:child {:type :ref}}})
        e (try
            (store/transact conn [:db/add 1 :child 999])
            nil
            (catch e e))]
    (is (some? e) ":ref to nonexistent eid throws")
    (is (some? (re-find #"dangling-ref" (pr-str (ex-data e))))
        "ex-data carries ::dangling-ref tag")
    (is (= #{} (store/entities (store/db conn))) "no facts applied on throw")
    (is (= 0 (:tx (store/db conn))) "tx counter not advanced on throw")))

(deftest store-reverse-index-built
  ;; store/referring returns the set of source eids whose value for the
  ;; given ref-attr equals the target eid.
  (let [conn (store/open nil {:schema {:child {:type :ref}}})
        _ (store/transact conn [[:db/add 1 :name "Parent"]
                                [:db/add 2 :child 1]
                                [:db/add 3 :child 1]])
        db (store/db conn)]
    (is (= #{2 3} (store/referring db :child 1)))
    (is (= #{} (store/referring db :child 999)) "no refs to absent eid")))

(deftest store-referred-by-returns-map
  ;; store/referred-by returns a map of {ref-attr #{source-eids}} for
  ;; every ref attribute pointing at the given eid.
  (let [conn (store/open nil {:schema {:child {:type :ref}
                                       :friend {:type :ref}}})
        _ (store/transact conn [[:db/add 1 :name "Target"]
                                [:db/add 2 :child 1]
                                [:db/add 3 :child 1]
                                [:db/add 4 :friend 1]])
        db (store/db conn)]
    (is (= {:child #{2 3} :friend #{4}} (store/referred-by db 1)))
    (is (= {} (store/referred-by db 999)) "absent target has empty map")))

(deftest store-retract-target-nils-refs
  ;; When an entity ceases to exist (its last attribute retracted), ref
  ;; attributes in other entities that pointed at it are nilled.
  (let [conn (store/open nil {:schema {:child {:type :ref}}})
        _ (store/transact conn [[:db/add 1 :name "Parent"]
                                [:db/add 2 :name "Holder"]
                                [:db/add 2 :child 1]])
        _ (store/transact conn [:db/retract 1 :name])
        db (store/db conn)]
    (is (not (store/entity-exists? db 1)) "target fully retracted")
    (is (nil? (store/read db 2 :child)) "dangling ref nilled in source")
    (is (= #{} (store/referring db :child 1)) "reverse index drops the link")))

(deftest store-ref-with-many-cardinality
  ;; A :ref with :cardinality :many holds a set of eids and the reverse
  ;; index tracks every source for each target.
  (let [conn (store/open nil {:schema {:members {:type :ref
                                                :cardinality :many}}})
        _ (store/transact conn [[:db/add 1 :name "A"]
                                [:db/add 2 :name "B"]
                                [:db/add 3 :name "C"]
                                [:db/add 10 :members 1]
                                [:db/add 10 :members 2]
                                [:db/add 20 :members 1]])
        db (store/db conn)]
    (is (= #{1 2} (store/read db 10 :members)))
    (is (= #{10 20} (store/referring db :members 1)))
    (is (= #{10} (store/referring db :members 2)))))

;; ---------------------------------------------------------------------------
;; Pull
;; ---------------------------------------------------------------------------

(deftest store-pull-flat-attr
  ;; A bare keyword spec returns that single attribute's value.
  (let [conn (store/open)
        _ (store/transact conn {1 {:name "Alice" :age 30}})
        db (store/db conn)]
    (is (= {:db/id 1 :name "Alice"} (store/pull db [:name] 1)))))

(deftest store-pull-wildcard
  ;; `*` returns every attribute of the entity.
  (let [conn (store/open)
        _ (store/transact conn {1 {:name "Alice" :age 30}})
        db (store/db conn)]
    (is (= {:db/id 1 :name "Alice" :age 30} (store/pull db '[*] 1)))))

(deftest store-pull-map-spec-ref
  ;; A map-spec {attr sub-pattern} recurses into a :ref attribute. The
  ;; :as rename applies at the key position; :many refs yield a seq.
  (let [conn (store/open nil {:schema {:child {:type :ref
                                              :cardinality :many}}})
        _ (store/transact conn {1 {:name "Parent"}
                                2 {:name "Alice"}
                                3 {:name "Bob"}})
        _ (store/transact conn [[:db/add 1 :child 2]
                                [:db/add 1 :child 3]])
        db (store/db conn)
        result (store/pull db [:name {[:child :as :kids] [:name]}] 1)]
    (is (= {:db/id 1 :name "Parent"} (dissoc result :kids)))
    (is (= #{{:db/id 2 :name "Alice"} {:db/id 3 :name "Bob"}}
           (set (:kids result))))))

(deftest store-pull-recursion-limit
  ;; A positive number in map-spec position bounds recursion depth; the
  ;; leaf returns only :db/id once the budget is spent.
  (let [conn (store/open nil {:schema {:parent {:type :ref}}})
        _ (store/transact conn [[:db/add 1 :name "Alice"]
                                [:db/add 2 :name "Bob"]
                                [:db/add 3 :name "Carol"]
                                [:db/add 1 :parent 2]
                                [:db/add 2 :parent 3]])
        db (store/db conn)]
    (is (= {:db/id 1 :parent {:db/id 2}}
           (store/pull db [{:parent 1}] 1))
        "limit 1 stops at :db/id on the first descent")))

(deftest store-pull-unlimited-recursion-cycle
  ;; `...` allows unlimited recursion and is cycle-safe: a revisit
  ;; yields only :db/id instead of looping forever.
  (let [conn (store/open nil {:schema {:friend {:type :ref}}})
        _ (store/transact conn [[:db/add 1 :name "Alice"]
                                [:db/add 2 :name "Bob"]
                                [:db/add 1 :friend 2]
                                [:db/add 2 :friend 1]])
        db (store/db conn)]
    (is (= {:db/id 1 :name "Alice"
            :friend {:db/id 2 :name "Bob"
                     :friend {:db/id 1}}}
           (store/pull db '[:name {:friend ...}] 1))
        "cycle terminates with :db/id on revisit")))

(deftest store-pull-as-rename
  ;; [:attr :as name] renames the attribute in the output map.
  (let [conn (store/open)
        _ (store/transact conn {1 {:name "Alice"}})
        db (store/db conn)]
    (is (= {:db/id 1 :fullName "Alice"}
           (store/pull db [[:name :as :fullName]] 1)))))

(deftest store-pull-limit-many
  ;; [:attr :limit N] bounds a cardinality-:many attribute to N values.
  (let [conn (store/open nil {:schema {:tags {:cardinality :many}}})
        _ (store/transact conn {1 {:tags #{:a :b :c :d}}})
        db (store/db conn)
        result (store/pull db [[:tags :limit 2]] 1)]
    (is (= 2 (count (:tags result))) "at most N values")
    (is (every? #{:a :b :c :d} (:tags result)) "values come from the source")))

(deftest store-pull-default
  ;; [:attr :default v] supplies v when the attribute is absent.
  (let [conn (store/open)
        _ (store/transact conn {1 {:name "Alice"}})
        db (store/db conn)]
    (is (= {:db/id 1 :name "Alice" :age 21}
           (store/pull db [:name [:age :default 21]] 1))
        "missing attr appears with its default")))

(deftest store-pull-many
  ;; pull-many maps pull across a seq of eids, preserving order.
  (let [conn (store/open)
        _ (store/transact conn {1 {:name "Alice"}
                                2 {:name "Bob"}
                                3 {:name "Carol"}})
        db (store/db conn)
        result (store/pull-many db [:name] [1 2 3])]
    (is (= 3 (count result)))
    (is (= #{{:db/id 1 :name "Alice"}
             {:db/id 2 :name "Bob"}
             {:db/id 3 :name "Carol"}}
           (set result)))))

(deftest store-pull-missing-attr-omitted
  ;; Attributes with no value on the entity are absent from the result
  ;; (not nil).
  (let [conn (store/open)
        _ (store/transact conn {1 {:name "Alice"}})
        db (store/db conn)]
    (is (= {:db/id 1 :name "Alice"}
           (store/pull db [:name :age :email] 1))
        "absent attrs :age and :email are omitted")))

(deftest store-pull-nonexistent-entity
  ;; Pulling an eid that does not exist returns nil.
  (let [conn (store/open)
        _ (store/transact conn {1 {:name "Alice"}})
        db (store/db conn)]
    (is (nil? (store/pull db [:name] 999)))))

(deftest store-pull-includes-db-id
  ;; The result map always carries :db/id, including under a wildcard.
  (let [conn (store/open)
        _ (store/transact conn {42 {:name "Alice" :age 30}})
        db (store/db conn)]
    (is (= 42 (:db/id (store/pull db [*] 42))))))

;; ---------------------------------------------------------------------------
;; Aggregates + :with
;;
;; ---------------------------------------------------------------------------

(deftest store-q-aggregate-count
  ;; (count ?e) in find position counts the bindings in each group.
  ;; With no group-by var, all bindings collapse to one group.
  (let [conn (store/open)
        _ (store/transact conn {1 {:name "Alice"}
                                2 {:name "Bob"}
                                3 {:name "Carol"}})
        db (store/db conn)]
    (is (= #{[3]}
           (store/q db '[:find (count ?e) :where [?e :name ?n]]))
        "count over all entities returns a single [3] tuple")))

(deftest store-q-aggregate-sum-with-group-by
  ;; A non-aggregate find var becomes a group-by key; (sum ?v) sums
  ;; within each group.
  (let [conn (store/open)
        _ (store/transact conn {1 {:dept "A" :amount 100}
                                2 {:dept "A" :amount 200}
                                3 {:dept "B" :amount 50}})
        db (store/db conn)]
    (is (= #{["A" 300] ["B" 50]}
           (store/q db '[:find ?dept (sum ?amount)
                         :where [?e :dept ?dept]
                                [?e :amount ?amount]])))))

(deftest store-q-aggregate-min-max
  ;; (min ?v) and (max ?v) return the extreme values as single tuples.
  (let [conn (store/open)
        _ (store/transact conn {1 {:age 30}
                                2 {:age 25}
                                3 {:age 35}})
        db (store/db conn)]
    (is (= #{[25]}
           (store/q db '[:find (min ?age) :where [?e :age ?age]])))
    (is (= #{[35]}
           (store/q db '[:find (max ?age) :where [?e :age ?age]])))))

(deftest store-q-aggregate-min-max-empty
  ;; (min ?v) / (max ?v) over an empty result set no longer crash on an
  ;; arity-0 reduce. As scalars they return nil (Datomic nil-for-empty);
  ;; as relations they share the uniform empty-aggregate default-row path
  ;; every other aggregate uses (count -> [0]), now yielding nil instead
  ;; of throwing MAR002. The populated db ensures the empty set comes from
  ;; the where-clause matching no entity, not from an empty store.
  (let [conn (store/open)
        _ (store/transact conn {1 {:age 30}})
        db (store/db conn)]
    (is (nil? (store/q db '[:find (min ?age) . :where [?e :no-such-attr ?age]])))
    (is (nil? (store/q db '[:find (max ?age) . :where [?e :no-such-attr ?age]])))
    (is (= #{[nil]}
           (store/q db '[:find (min ?age) :where [?e :no-such-attr ?age]])))
    (is (= #{[0]}
           (store/q db '[:find (count ?age) :where [?e :no-such-attr ?age]])))))

(deftest store-q-aggregate-avg
  ;; (avg ?v) returns the arithmetic mean of the bindings.
  (let [conn (store/open)
        _ (store/transact conn {1 {:age 30}
                                2 {:age 25}
                                3 {:age 35}})
        db (store/db conn)]
    (is (= #{[30]}
           (store/q db '[:find (avg ?age) :where [?e :age ?age]]))
        "(30 + 25 + 35) / 3 = 30")))

(deftest store-q-aggregate-avg-empty
  ;; (avg ?v) over an empty result set returns nil, mirroring the
  ;; nil-for-empty rule established for min/max in
  ;; store-q-aggregate-min-max-empty. avg is sum/count and Datomic
  ;; returns nil for a scalar aggregate over zero bindings; the
  ;; previous 0 was indistinguishable from a real "average happens to
  ;; be zero" and was the wrong type for a query that matched nothing.
  ;; The populated db ensures the empty set comes from the where-clause
  ;; matching no entity, not from an empty store.
  (let [conn (store/open)
        _ (store/transact conn {1 {:age 30}})
        db (store/db conn)]
    (is (nil? (store/q db '[:find (avg ?age) . :where [?e :no-such-attr ?age]])))
    (is (= #{[nil]}
            (store/q db '[:find (avg ?age) :where [?e :no-such-attr ?age]])))))

(deftest store-q-aggregate-distinct
  ;; (distinct ?v) returns the set of distinct values as one tuple.
  (let [conn (store/open)
        _ (store/transact conn {1 {:dept "A"}
                                2 {:dept "A"}
                                3 {:dept "B"}})
        db (store/db conn)]
    (is (= #{[#{"A" "B"}]}
           (store/q db '[:find (distinct ?dept)
                         :where [?e :dept ?dept]])))))

(deftest store-q-with-clause
  ;; :with ?var groups by ?var but does NOT return it. Without :with,
  ;; (count ?e) over all entities would be a single [3] tuple; with
  ;; :with ?dept it groups per dept and returns each count.
  (let [conn (store/open)
        _ (store/transact conn {1 {:dept "A"}
                                2 {:dept "A"}
                                3 {:dept "B"}})
        db (store/db conn)]
    (is (= #{[2] [1]}
           (store/q db '[:find (count ?e) :with ?dept
                         :where [?e :dept ?dept]]))
        "groups by ?dept (counts 2 and 1) but omits ?dept from results")))

(deftest store-q-mixed-aggregate-scalar
  ;; Find position carries both a scalar var (group-by key) and an
  ;; aggregate expression; each group yields one tuple.
  (let [conn (store/open)
        _ (store/transact conn {1 {:role :admin :amount 10}
                                2 {:role :admin :amount 20}
                                3 {:role :user  :amount 5}})
        db (store/db conn)]
    (is (= #{[:admin 2] [:user 1]}
           (store/q db '[:find ?role (count ?e)
                         :where [?e :role ?role]]))
        "scalar ?role groups, (count ?e) aggregates per group")))

(deftest store-q-aggregate-empty-result
  ;; An aggregate over an empty result set returns the empty-group
  ;; value: count is 0, not an empty result set.
  (let [conn (store/open)
        _ (store/transact conn {1 {:name "Alice"}})
        db (store/db conn)]
    (is (= #{[0]}
           (store/q db '[:find (count ?e)
                         :where [?e :name "Nonexistent"]]))
        "count over zero bindings yields [0]")))

;; ---------------------------------------------------------------------------
;; Find specs
;;
;; ---------------------------------------------------------------------------

(deftest store-q-find-coll-spec
  ;; :find [?e ...] returns a collection of single values (not a set of
  ;; tuples).
  (let [conn (store/open)
        _ (store/transact conn {1 {:name "Alice"}
                                2 {:name "Bob"}
                                3 {:name "Carol"}})
        db (store/db conn)
        result (store/q db '[:find [?e ...] :where [?e :name ?n]])]
    (is (coll? result) "returns a collection")
    (is (= #{1 2 3} (set result)) "of single values, not tuples")
    (is (not (set? result)) "a seq, not a set of tuples")))

(deftest store-q-find-scalar-spec
  ;; :find ?e . returns a single scalar value, nil when empty.
  (let [conn (store/open)
        _ (store/transact conn {1 {:name "Alice"}
                                2 {:name "Bob"}})
        db (store/db conn)]
    (is (= 1
           (store/q db '[:find ?e . :where [?e :name "Alice"]])))
    (is (nil?
          (store/q db '[:find ?e . :where [?e :name "Nonexistent"]]))
        "scalar returns nil for empty result")))

(deftest store-q-find-tuple-spec
  ;; :find [?e ?n] returns a single tuple, nil when empty.
  (let [conn (store/open)
        _ (store/transact conn {1 {:name "Alice" :age 30}
                                2 {:name "Bob"   :age 25}})
        db (store/db conn)]
    (is (= [1 "Alice"]
           (store/q db '[:find [?e ?name]
                         :where [?e :name ?name]
                                [?e :age 30]])))
    (is (nil?
          (store/q db '[:find [?e ?name]
                        :where [?e :name ?name]
                               [?e :age 999]]))
        "tuple returns nil for empty result")))

(deftest store-q-find-rel-spec-unchanged
  ;; :find ?a ?b (bare vars) still returns a set of tuples.
  (let [conn (store/open)
        _ (store/transact conn {1 {:name "Alice"}
                                2 {:name "Bob"}})
        db (store/db conn)]
    (is (= #{[1 "Alice"] [2 "Bob"]}
           (store/q db '[:find ?e ?name :where [?e :name ?name]]))
        "relation spec (bare vars) preserves existing behavior")))

(deftest store-q-find-spec-with-aggregate
  ;; A find spec combines with an aggregate expression: scalar spec on
  ;; (count ?e) returns a bare number.
  (let [conn (store/open)
        _ (store/transact conn {1 {:age 30}
                                2 {:age 25}
                                3 {:age 35}})
        db (store/db conn)]
    (is (= 3
           (store/q db '[:find (count ?e) . :where [?e :age ?age]]))
        "scalar spec unwraps the aggregate tuple to a bare value")))

;; ---------------------------------------------------------------------------
;; Streaming query (qseq)
;;
;; (set (qseq ...)).
;; ---------------------------------------------------------------------------

(deftest store-qseq-returns-lazy-seq
  ;; qseq returns something that satisfies seq?.
  (let [conn (store/open)
        _ (store/transact conn {1 {:name "Alice"}
                                2 {:name "Bob"}})
        db (store/db conn)]
    (is (seq? (store/qseq db '[:find ?e :where [?e :name ?n]]))
        "qseq returns a seq")))

(deftest store-qseq-results-match-q
  ;; Collecting qseq into a set yields the same result as q.
  (let [conn (store/open)
        _ (store/transact conn {1 {:name "Alice"}
                                2 {:name "Bob"}})
        db (store/db conn)
        query '[:find ?e ?name :where [?e :name ?name]]]
    (is (= (store/q db query)
           (set (store/qseq db query)))
        "qseq collected into a set equals q")))

(deftest store-qseq-with-aggregates
  ;; qseq supports aggregate find expressions.
  (let [conn (store/open)
        _ (store/transact conn {1 {:age 30}
                                2 {:age 25}
                                3 {:age 35}})
        db (store/db conn)]
    (is (= #{[3]}
           (set (store/qseq db '[:find (count ?e) :where [?e :age ?age]])))
        "qseq yields aggregate tuples")))

(deftest store-qseq-with-find-spec
  ;; qseq supports find specs; the collection spec yields single values
  ;; lazily.
  (let [conn (store/open)
        _ (store/transact conn {1 {:name "Alice"}
                                2 {:name "Bob"}
                                3 {:name "Carol"}})
        db (store/db conn)
        result (store/qseq db '[:find [?e ...] :where [?e :name ?n]])]
    (is (seq? result) "coll spec via qseq is a seq")
    (is (= #{1 2 3} (set result)) "yields single values")))

(deftest store-qseq-is-lazy
  ;; qseq does not realize all results eagerly: first returns a valid
  ;; tuple without needing to force the whole seq.
  (let [conn (store/open)
        _ (store/transact conn {1 {:name "Alice"}
                                2 {:name "Bob"}
                                3 {:name "Carol"}})
        db (store/db conn)
        first-elem (first (store/qseq db '[:find ?e :where [?e :name ?n]]))]
    (is (some? first-elem) "first of qseq returns a value")
    (is (vector? first-elem) "first element is a tuple")))

;; ---------------------------------------------------------------------------
;; Attribute predicates
;;
;; :db/add value is passed to each predicate (resolved via resolve) AFTER
;; the type check and BEFORE the fact is applied. Falsy return aborts
;; the whole tx tagged ::attr-pred-failed.
;; ---------------------------------------------------------------------------

(defn store-pred-positive-num
  "Test helper attr pred: truthy when v is a positive number."
  [v]
  (and (number? v) (pos? v)))

(defn store-pred-non-empty-str
  "Test helper attr pred: truthy when v is a non-empty string."
  [v]
  (and (string? v) (pos? (count v))))

(defn store-pred-always-reject
  "Test helper attr pred: always returns false."
  [_]
  false)

(deftest store-attr-pred-accepts-valid
  (let [conn (store/open nil {:schema {:n {:type :long
                                           :preds [store-pred-positive-num]}}})
        _ (store/transact conn [:db/add 1 :n 5])
        db (store/db conn)]
    (is (= 5 (store/read db 1 :n)) "pred-satisfying value accepted")))

(deftest store-attr-pred-rejects-invalid
  (let [conn (store/open nil {:schema {:n {:type :long
                                           :preds [store-pred-positive-num]}}})
        e (try
            (store/transact conn [:db/add 1 :n -1])
            nil
            (catch e e))]
    (is (some? e) "pred rejection throws")
    (is (some? (re-find #"attr-pred-failed" (pr-str (ex-data e))))
        "ex-data carries ::attr-pred-failed tag")
    (is (= #{} (store/entities (store/db conn))) "no facts applied on throw")
    (is (= 0 (:tx (store/db conn))) "tx counter not advanced")))

(deftest store-attr-pred-multiple
  ;; Multiple preds on one attr: ALL must pass.
  (let [conn (store/open nil {:schema {:s {:type :string
                                           :preds [store-pred-non-empty-str
                                                   'store-pred-always-reject]}}})
        e (try
            (store/transact conn [:db/add 1 :s "hi"])
            nil
            (catch e e))]
    (is (some? e) "second pred failing throws")
    (is (some? (re-find #"attr-pred-failed" (pr-str (ex-data e))))
        "failure reports the pred that rejected")))

(deftest store-attr-pred-runs-after-type-check
  ;; The type check fires first. A string value for :long must surface as
  ;; the type error, NOT as an attr-pred-failed, even though the pred
  ;; would also reject.
  (let [conn (store/open nil {:schema {:n {:type :long
                                           :preds [store-pred-always-reject]}}})
        e (try
            (store/transact conn [:db/add 1 :n "not a number"])
            nil
            (catch e e))]
    (is (some? e) "type mismatch throws")
    (is (nil? (re-find #"attr-pred-failed" (pr-str (ex-data e))))
        "type error observed, pred never reached")))

(deftest store-attr-pred-tx-atomic
  ;; A pred failure on any one fact in a multi-fact tx aborts ALL facts.
  (let [conn (store/open nil {:schema {:n {:type :long
                                           :preds [store-pred-positive-num]}}})
        e (try
            (store/transact conn [[:db/add 1 :n 5]
                                  [:db/add 2 :n -1]])
            nil
            (catch e e))]
    (is (some? e) "pred failure on second fact throws")
    (is (= #{} (store/entities (store/db conn))) "first fact NOT applied (atomic)")
    (is (= 0 (:tx (store/db conn))) "tx not advanced")))

(deftest store-attr-pred-many-cardinality
  ;; Pred is checked per member for :cardinality :many attrs.
  (let [conn (store/open nil {:schema {:tags {:cardinality :many
                                              :preds [store-pred-non-empty-str]}}})
        _ (store/transact conn [[:db/add 1 :tags "x"]
                                [:db/add 1 :tags "y"]])
        db-ok (store/db conn)
        e (try
            (store/transact conn [:db/add 1 :tags ""])
            nil
            (catch e e))]
    (is (= #{"x" "y"} (store/read db-ok 1 :tags)) "valid members accepted")
    (is (some? e) "pred rejection on a :many member throws")
    (is (some? (re-find #"attr-pred-failed" (pr-str (ex-data e)))))))

;; ---------------------------------------------------------------------------
;; Entity specs (:db/ensure)
;;
;; Spec-first. Entity specs are registered at open time under
;; :entity-specs. tx-data map form carries a virtual :db/ensure spec-name
;; key. After all facts apply, each ensured entity is checked for
;; required-attrs and preds. Failure aborts tagged ::entity-spec-failed.
;; ---------------------------------------------------------------------------

(defn store-spec-email-has-at
  "Test helper entity pred: truthy when entity's :email contains @.
  Entity preds take (db eid) and return truthy/falsy."
  [db eid]
  (let [email (store/read db eid :email)]
    (and (string? email)
         (boolean (re-find #"@" email)))))

(defn store-spec-always-reject
  "Test helper entity pred: always falsy."
  [_ _]
  false)

(def store-es-schema
  "Shared schema for entity-spec tests."
  {:name {:type :string} :email {:type :string} :note {:type :string}})

(deftest store-entity-spec-required-attrs-satisfied
  (let [conn (store/open nil {:schema store-es-schema
                             :entity-specs {:user {:required-attrs [:name :email]}}})
        _ (store/transact conn {1 {:db/ensure :user :name "Alice" :email "a@x.com"}})
        db (store/db conn)]
    (is (= "Alice" (store/read db 1 :name)))
    (is (= "a@x.com" (store/read db 1 :email)))
    (is (nil? (:db/ensure (store/entity db 1)))
        ":db/ensure is virtual, not stored on the entity")))

(deftest store-entity-spec-required-attrs-missing
  (let [conn (store/open nil {:schema store-es-schema
                             :entity-specs {:user {:required-attrs [:name :email]}}})
        e (try
            (store/transact conn {1 {:db/ensure :user :name "Alice"}})
            nil
            (catch e e))]
    (is (some? e) "missing required attr throws")
    (is (some? (re-find #"entity-spec-failed" (pr-str (ex-data e))))
        "ex-data carries ::entity-spec-failed tag")
    (is (= #{} (store/entities (store/db conn))) "no facts applied")))

(deftest store-entity-spec-pred-passes
  (let [conn (store/open nil {:schema store-es-schema
                             :entity-specs {:user {:preds [store-spec-email-has-at]}}})
        _ (store/transact conn {1 {:db/ensure :user :name "Alice" :email "a@x.com"}})
        db (store/db conn)]
    (is (= "a@x.com" (store/read db 1 :email)) "pred-satisfying entity accepted")))

(deftest store-entity-spec-pred-fails
  (let [conn (store/open nil {:schema store-es-schema
                             :entity-specs {:user {:preds [store-spec-always-reject]}}})
        e (try
            (store/transact conn {1 {:db/ensure :user :name "Alice" :email "a@x.com"}})
            nil
            (catch e e))]
    (is (some? e) "failing entity pred throws")
    (is (some? (re-find #"entity-spec-failed" (pr-str (ex-data e)))))
    (is (= #{} (store/entities (store/db conn))) "no facts applied")))

(deftest store-entity-spec-on-new-entity
  ;; Ensure works when the tx creates the entity from scratch; the spec
  ;; is checked against the working entities view after all facts apply.
  (let [conn (store/open nil {:schema store-es-schema
                             :entity-specs {:user {:required-attrs [:name :email]}}})
        _ (store/transact conn {1 {:db/ensure :user :name "Alice" :email "a@x.com"}})
        db (store/db conn)]
    (is (= #{1} (store/entities db)) "entity created")))

(deftest store-entity-spec-atomic
  ;; Spec failure aborts the whole tx, including facts on entities that
  ;; are NOT under any spec.
  (let [conn (store/open nil {:schema store-es-schema
                             :entity-specs {:user {:required-attrs [:name :email]}}})
        e (try
            (store/transact conn {1 {:db/ensure :user :name "Alice"}
                                  2 {:note "unrelated"}})
            nil
            (catch e e))]
    (is (some? e) "entity spec failure throws")
    (is (= #{} (store/entities (store/db conn))) "no facts applied (atomic)")
    (is (= 0 (:tx (store/db conn))) "tx not advanced")))

;; ---------------------------------------------------------------------------
;; Migration API
;;
;; schema, validates existing facts, optionally coerces, and publishes
;; the result. Returns {:db-after new-db :violations [...] :tx N}.
;; ---------------------------------------------------------------------------

(defn store-mig-coerce-to-long
  "Test helper coerce-fn: maps any value to the long 42."
  [_v]
  42)

(deftest store-migrate-add-new-attr
  (let [conn (store/open nil {:schema {:name {:type :string}}})
        _ (store/transact conn [:db/add 1 :name "Alice"])
        r (store/migrate conn {:name {:type :string} :email {:type :string}})
        db (:db-after r)]
    (is (= [] (:violations r)) "no violations adding a new attr")
    (is (some? (:tx r)) "returns tx number")
    (is (= "Alice" (store/read db 1 :name)) "existing data preserved")
    ;; New schema is live: :email is now transactable.
    (store/transact conn [:db/add 1 :email "a@x.com"])
    (is (= "a@x.com" (store/read (store/db conn) 1 :email)))))

(deftest store-migrate-tighten-type-throws
  ;; Tightening :any -> :long when existing data is non-long throws
  ;; ::migration-conflict without :force.
  (let [conn (store/open nil {:schema {:n {}}})
        _ (store/transact conn [:db/add 1 :n "not a number"])
        e (try
            (store/migrate conn {:n {:type :long}})
            nil
            (catch e e))]
    (is (some? e) "violations throw without :force")
    (is (some? (re-find #"migration-conflict" (pr-str (ex-data e))))
        "ex-data carries ::migration-conflict tag")
    (is (= "not a number" (store/read (store/db conn) 1 :n))
        "existing db unchanged on throw")))

(deftest store-migrate-tighten-type-with-coerce
  ;; coerce-fn transforms non-conforming values before validation; the
  ;; migration then succeeds and the coerced value is visible.
  (let [conn (store/open nil {:schema {:n {}}})
        _ (store/transact conn [:db/add 1 :n "42"])
        r (store/migrate conn {:n {:type :long}}
                         {:coerce {:n store-mig-coerce-to-long}})
        db (:db-after r)]
    (is (= [] (:violations r)) "coerce fixed the violation")
    (is (= 42 (store/read db 1 :n)) "coerced value visible in entities")))

(deftest store-migrate-add-indexes
  ;; Adding indexes to existing attrs builds the index as part of the
  ;; migration; find-by works immediately afterward.
  (let [conn (store/open nil {:schema {:email {:type :string}}})
        _ (store/transact conn [:db/add 1 :email "a@x.com"])
        _ (store/migrate conn {:email {:type :string}} {:indexes #{:email}})
        db (store/db conn)]
    (is (= 1 (store/find-by db :email "a@x.com")) "index built during migration")))

(deftest store-migrate-then-transact
  ;; After migration the new schema is active for new transactions,
  ;; including type tightening.
  (let [conn (store/open nil {:schema {:name {:type :string}}})
        _ (store/migrate conn {:name {:type :string} :age {:type :long}})
        _ (store/transact conn [:db/add 1 :name "Alice"])
        _ (store/transact conn [:db/add 1 :age 30])
        db (store/db conn)]
    (is (= 30 (store/read db 1 :age)) "new attr transactable after migration"))
  (let [conn (store/open nil {:schema {:n {}}})
        _ (store/migrate conn {:n {:type :long}})
        e (try
            (store/transact conn [:db/add 1 :n "not a number"])
            nil
            (catch e e))]
    (is (some? e) "tightened type enforced on new txs")))

(deftest store-migrate-durable
  ;; Migration persists across checkpoint+reopen: the new schema is
  ;; active on the reopened connection.
  (rm-rf store-test-dir)
  (mkdir-p store-test-dir)
  (let [path (str store-test-dir "/mig.db")]
    (try
      (let [conn (store/open path {:schema {:name {:type :string}}})
            _ (store/transact conn [:db/add 1 :name "Alice"])
            _ (store/migrate conn {:name {:type :string} :email {:type :string}})
            _ (store/checkpoint conn)
            _ (store/close conn)
            conn2 (store/open path)
            _ (store/transact conn2 [:db/add 1 :email "a@x.com"])
            db (store/db conn2)]
        (is (= "Alice" (store/read db 1 :name)) "pre-migration value intact")
        (is (= "a@x.com" (store/read db 1 :email)) "post-migration attr usable")
        (store/close conn2))
      (finally
        (rm-rf store-test-dir)))))

;; ---------------------------------------------------------------------------
;; Per-attribute :noHistory
;;
;; for no-history attrs are applied to entities but NOT appended to :log;
;; as-of/since/history for these attrs return the current value at all
;; points. Indexes are maintained normally.
;; ---------------------------------------------------------------------------

(deftest store-no-history-attr-not-in-log
  (let [conn (store/open nil {:schema {:name {:type :string}
                                       :ephemeral {:type :string :no-history true}}})
        _ (store/transact conn [:db/add 1 :name "Alice"])
        _ (store/transact conn [:db/add 1 :ephemeral "v0"])
        log (:log (store/db conn))]
    (is (= 1 (count (filter #(= (:a %) :name) log))) ":name logged normally")
    (is (zero? (count (filter #(= (:a %) :ephemeral) log)))
        ":ephemeral facts NOT appended to log")))

(deftest store-no-history-attr-readable
  (let [conn (store/open nil {:schema {:ephemeral {:type :string :no-history true}}})
        _ (store/transact conn [:db/add 1 :ephemeral "v0"])
        _ (store/transact conn [:db/add 1 :ephemeral "v1"])
        db (store/db conn)]
    (is (= "v1" (store/read db 1 :ephemeral)) "current value readable")
    (is (= "v1" (:ephemeral (store/entity db 1))) "visible in entity map")))

(deftest store-no-history-as-of-returns-current
  ;; With :no-history true, as-of at any prior tx returns the CURRENT
  ;; value rather than the historical value (there is no history).
  (let [conn (store/open nil {:schema {:a {:type :string}
                                       :eph {:type :string :no-history true}}})
        _ (store/transact conn [:db/add 1 :a "a0"])      ;; tx 0
        _ (store/transact conn [:db/add 1 :eph "e0"])    ;; tx 1
        _ (store/transact conn [:db/add 1 :eph "e1"])    ;; tx 2
        db (store/db conn)]                              ;; :tx 3
    ;; Normal attr honors tx boundaries.
    (is (nil? (store/read (store/as-of db 0) 1 :a)) "normal attr absent before its tx")
    ;; no-history attr returns the CURRENT value at every as-of point.
    (is (= "e1" (store/read (store/as-of db 3) 1 :eph))
        "no-history as-of at current tx returns current value")
    (is (= "e1" (store/read (store/as-of db 1) 1 :eph))
        "no-history as-of at tx 1 (before e1 was written) still returns current")))

(deftest store-no-history-index-maintained
  ;; Indexes are maintained normally for :no-history attrs.
  (let [conn (store/open nil {:schema {:email {:type :string :no-history true}}
                              :indexes #{:email}})
        _ (store/transact conn [:db/add 1 :email "a@x.com"])
        _ (store/transact conn [:db/add 1 :email "b@x.com"])
        db (store/db conn)]
    (is (= 1 (store/find-by db :email "b@x.com")) "current value indexed")
    (is (nil? (store/find-by db :email "a@x.com")) "stale value dropped from index")))

;; ---------------------------------------------------------------------------
;; Retention makes as-of partial
;;
;; so as-of at an early tx returns a partial or empty view.
;; ---------------------------------------------------------------------------

(deftest store-as-of-with-retention-partial
  ;; With :keep-last 2, once the log exceeds the compaction threshold
  ;; the oldest facts are dropped. as-of at an early tx cannot
  ;; reconstruct the past and returns a partial/empty view. This
  ;; demonstrates why the as-of contract warns about retention.
  (let [conn (store/open nil {:history {:keep-last 2}})
        _ (store/transact conn [:db/add 1 :a :v0])   ;; tx 0
        _ (store/transact conn [:db/add 1 :a :v1])   ;; tx 1
        _ (store/transact conn [:db/add 1 :a :v2])   ;; tx 2
        _ (store/transact conn [:db/add 1 :a :v3])   ;; tx 3
        _ (store/transact conn [:db/add 1 :a :v4])   ;; tx 4
        _ (store/transact conn [:db/add 1 :a :v5])   ;; tx 5
        db (store/db conn)]
    (is (= :v5 (store/read db 1 :a)) "current value preserved")
    (is (< (count (:log db)) 6)
        "log has been compacted below the full 6 facts")
    (let [as-of-0 (store/as-of db 0)]
      (is (or (not (store/entity-exists? as-of-0 1))
              (nil? (store/read as-of-0 1 :a)))
          "as-of at tx 0 returns partial/empty view after retention"))))

;; ---------------------------------------------------------------------------
;; Parameterized queries queries
;;
;; Spec-first. Extra args after the query map positionally to :in vars.
;; ---------------------------------------------------------------------------
(deftest store-q-in-single
  ;; A single :in var is bound to the extra arg, narrowing the query.
  (let [conn (store/open)
        _ (store/transact conn {1 {:name "Alice"}
                                2 {:name "Bob"}})
        db (store/db conn)]
    (is (= #{[1]}
           (store/q db '[:find ?e :in ?n :where [?e :name ?n]] "Alice")))))

(deftest store-q-in-multiple
  ;; Multiple :in vars bind positionally to successive extra args.
  (let [conn (store/open)
        _ (store/transact conn {1 {:name "Alice" :age 30}
                                2 {:name "Bob"   :age 25}})
        db (store/db conn)]
    (is (= #{[1]}
           (store/q db '[:find ?e
                         :in ?n ?a
                         :where [?e :name ?n]
                                [?e :age ?a]]
                    "Alice" 30)))))

(deftest store-q-in-with-aggregate
  ;; :in binds a group key used by an aggregate expression.
  (let [conn (store/open)
        _ (store/transact conn {1 {:dept "A" :amount 100}
                                2 {:dept "A" :amount 200}
                                3 {:dept "B" :amount 50}})
        db (store/db conn)]
    (is (= #{["A" 300]}
           (store/q db '[:find ?dept (sum ?amount)
                         :in ?dept
                         :where [?e :dept ?dept]
                                [?e :amount ?amount]]
                    "A")))))

(deftest store-q-in-scalar-binding
  ;; :in combined with a scalar find spec unwraps the single result.
  (let [conn (store/open)
        _ (store/transact conn {1 {:name "Alice"}
                                2 {:name "Bob"}})
        db (store/db conn)]
    (is (= 1
           (store/q db '[:find ?e .
                         :in ?n
                         :where [?e :name ?n]]
                    "Alice")))))

(deftest store-qseq-in
  ;; qseq honors :in bindings just like q.
  (let [conn (store/open)
        _ (store/transact conn {1 {:name "Alice"}
                                2 {:name "Bob"}})
        db (store/db conn)]
    (is (= #{[1]}
           (set (store/qseq db '[:find ?e :in ?n :where [?e :name ?n]]
                            "Alice"))))))

;; ---------------------------------------------------------------------------
;; Retract entity
;;
;; Spec-first. [:db/retractEntity eid] retracts every attribute of the
;; entity, removing it entirely.
;; ---------------------------------------------------------------------------
(deftest store-retract-entity-all-attrs
  ;; retractEntity strips every attribute, dissolving the entity.
  (let [conn (store/open)
        _ (store/transact conn {1 {:name "Alice" :age 30 :email "a@x.com"}})
        _ (store/transact conn [:db/retractEntity 1])
        db (store/db conn)]
    (is (not (store/entity-exists? db 1)) "entity fully removed")
    (is (= #{} (store/entities db)))))

(deftest store-retract-entity-nonexistent
  ;; Retracting an absent eid is a no-op.
  (let [conn (store/open)
        _ (store/transact conn {1 {:name "Alice"}})
        _ (store/transact conn [:db/retractEntity 999])
        db (store/db conn)]
    (is (= #{1} (store/entities db)) "unrelated entity untouched")))

(deftest store-retract-entity-cleans-refs
  ;; Refs pointing at a retractEntity'd target are nilled (the existing
  ;; dangling-ref cleanup, exercised here via retractEntity).
  (let [conn (store/open nil {:schema {:child {:type :ref}}})
        _ (store/transact conn [[:db/add 1 :name "Parent"]
                                [:db/add 2 :name "Holder"]
                                [:db/add 2 :child 1]])
        _ (store/transact conn [:db/retractEntity 1])
        db (store/db conn)]
    (is (not (store/entity-exists? db 1)))
    (is (nil? (store/read db 2 :child)) "dangling ref nilled")))

(deftest store-retract-entity-returns-tx
  ;; retractEntity returns the normal transact result shape.
  (let [conn (store/open)
        _ (store/transact conn {1 {:name "Alice"}})
        r (store/transact conn [:db/retractEntity 1])]
    (is (contains? r :tx))
    (is (contains? r :db-after))
    (is (= (:db-after r) (store/db conn)))))

;; ---------------------------------------------------------------------------
;; Negation (not / not-join)
;;
;; Spec-first. (not [pattern]) filters out bindings where the nested
;; pattern matches. (not-join [vars] pattern...) takes the join vars
;; explicitly.
;; ---------------------------------------------------------------------------
(deftest store-q-not-basic
  ;; (not [?e :archived true]) keeps only entities lacking that fact.
  (let [conn (store/open)
        _ (store/transact conn {1 {:name "Alice" :archived true}
                                2 {:name "Bob"}})
        db (store/db conn)]
    (is (= #{[2]}
           (store/q db '[:find ?e
                         :where [?e :name ?n]
                                (not [?e :archived true])])))))

(deftest store-q-not-with-constant
  ;; (not [?e :status :active]) filters out active entities.
  (let [conn (store/open)
        _ (store/transact conn {1 {:status :active}
                                2 {:status :inactive}
                                3 {:status :active}})
        db (store/db conn)]
    (is (= #{[2]}
           (store/q db '[:find ?e
                         :where [?e :status ?s]
                                (not [?e :status :active])])))))

(deftest store-q-not-join
  ;; not-join names the join variable(s) explicitly.
  (let [conn (store/open)
        _ (store/transact conn {1 {:dept "A" :role :admin}
                                2 {:dept "A" :role :user}
                                3 {:dept "B" :role :user}})
        db (store/db conn)]
    (is (= #{[2] [3]}
           (store/q db '[:find ?e
                         :where [?e :dept ?d]
                                (not-join [?e] [?e :role :admin])])))))

(deftest store-q-not-empty-result
  ;; (not ...) matching every binding yields an empty result set.
  (let [conn (store/open)
        _ (store/transact conn {1 {:name "Alice"}
                                2 {:name "Bob"}})
        db (store/db conn)]
    (is (= #{}
           (store/q db '[:find ?e
                         :where [?e :name ?n]
                                (not [?e :name ?n])]))
        "not matching every binding returns empty")))

;; ---------------------------------------------------------------------------
;; Order-by
;;
;; Spec-first. :order-by ?var sorts the result ascending; appending
;; :desc sorts descending. Ordered results come back as a seq of tuples
;; (not a set).
;; ---------------------------------------------------------------------------
(deftest store-q-order-by-asc
  ;; Ascending order-by returns tuples in ascending value order.
  (let [conn (store/open)
        _ (store/transact conn {1 {:age 30}
                                2 {:age 25}
                                3 {:age 35}})
        db (store/db conn)]
    (is (= [[2 25] [1 30] [3 35]]
           (store/q db '[:find ?e ?age
                         :order-by ?age
                         :where [?e :age ?age]])))))

(deftest store-q-order-by-desc
  ;; :desc reverses the sort.
  (let [conn (store/open)
        _ (store/transact conn {1 {:age 30}
                                2 {:age 25}
                                3 {:age 35}})
        db (store/db conn)]
    (is (= [[3 35] [1 30] [2 25]]
           (store/q db '[:find ?e ?age
                         :order-by ?age :desc
                         :where [?e :age ?age]])))))

(deftest store-q-order-by-with-limit
  ;; order-by composes with a find collection spec, yielding ordered values.
  (let [conn (store/open)
        _ (store/transact conn {1 {:age 30}
                                2 {:age 25}
                                3 {:age 35}})
        db (store/db conn)
        result (store/q db '[:find [?e ...]
                             :order-by ?age
                             :where [?e :age ?age]])]
    (is (= [2 1 3] result))))

(deftest store-qseq-order-by
  ;; qseq preserves order-by ordering and is still a seq.
  (let [conn (store/open)
        _ (store/transact conn {1 {:age 30}
                                2 {:age 25}
                                3 {:age 35}})
        db (store/db conn)
        result (store/qseq db '[:find ?e ?age
                                :order-by ?age
                                :where [?e :age ?age]])]
    (is (seq? result))
    (is (= [[2 25] [1 30] [3 35]] result))))

;; ---------------------------------------------------------------------------
;; Sorted range queries index + find-by-range
;;
;; Spec-first. {:sorted-index true} on an attribute enables
;; (store/find-by-range db attr lo hi), returning eids whose value is in
;; [lo, hi], in ascending value order.
;; ---------------------------------------------------------------------------
(deftest store-sorted-index-range
  ;; find-by-range returns eids whose value falls in the inclusive range.
  (let [conn (store/open nil {:schema {:age {:type :long
                                              :sorted-index true}}})
        _ (store/transact conn [[:db/add 1 :age 25]
                                [:db/add 2 :age 30]
                                [:db/add 3 :age 35]
                                [:db/add 4 :age 40]])
        db (store/db conn)]
    (is (= #{2 3} (set (store/find-by-range db :age 30 35))))))

(deftest store-sorted-index-inclusive
  ;; Range bounds are inclusive on both ends.
  (let [conn (store/open nil {:schema {:age {:type :long
                                              :sorted-index true}}})
        _ (store/transact conn [[:db/add 1 :age 10]
                                [:db/add 2 :age 20]
                                [:db/add 3 :age 30]])
        db (store/db conn)]
    (is (= #{1 2 3} (set (store/find-by-range db :age 10 30))))))

(deftest store-sorted-index-empty
  ;; A range with no matches returns an empty seq.
  (let [conn (store/open nil {:schema {:age {:type :long
                                              :sorted-index true}}})
        _ (store/transact conn [[:db/add 1 :age 10]
                                [:db/add 2 :age 20]])
        db (store/db conn)]
    (is (empty? (store/find-by-range db :age 100 200)))))

(deftest store-find-by-range-rejects-mixed-types
  ;; Bounds and attribute values must be mutually comparable. A numeric
  ;; range over a string attribute used to throw a raw ClassCastException
  ;; from inside (>= v lo); it must surface a classified error instead so
  ;; the caller can tell a misuse from a runtime bug.
  (let [conn (store/open nil {:schema {:name {:type :string}}})
        _ (store/transact conn [[:db/add 1 :name "Alice"]
                                [:db/add 2 :name "Bob"]])
        db (store/db conn)
        e (try (store/find-by-range db :name 1 10) (catch e e))]
    (is (some? e) "mixed-type range throws")
    (is (= :range-type-mismatch (:reason (ex-data e)))
        "classified :range-type-mismatch, not a raw ClassCastException")))

(deftest store-sorted-index-ordered
  ;; find-by-range results come back in ascending value order.
  (let [conn (store/open nil {:schema {:age {:type :long
                                              :sorted-index true}}})
        _ (store/transact conn [[:db/add 1 :age 40]
                                [:db/add 2 :age 10]
                                [:db/add 3 :age 30]
                                [:db/add 4 :age 20]])
        db (store/db conn)
        eids (store/find-by-range db :age 10 40)]
    (is (= [10 20 30 40]
           (map #(store/read db % :age) eids)))))

;; ---------------------------------------------------------------------------
;; Transaction result detail detail (:tx-data)
;;
;; Spec-first. transact's return map gains :tx-data, a seq of
;; {:e :a :v :op} maps describing what was asserted/retracted.
;; ---------------------------------------------------------------------------
(deftest store-tx-data-add
  ;; A single :db/add yields one tx-data entry with the asserted e/a/v.
  (let [conn (store/open)
        r (store/transact conn [:db/add 1 :name "Alice"])
        td (:tx-data r)]
    (is (= 1 (count td)) "one tx-data entry")
    (is (= {:e 1 :a :name :v "Alice" :op :db/add} (first td)))))

(deftest store-tx-data-retract
  ;; A :db/retract surfaces as a :db/retract op in tx-data.
  (let [conn (store/open)
        _ (store/transact conn [:db/add 1 :name "Alice"])
        r (store/transact conn [:db/retract 1 :name "Alice"])
        td (:tx-data r)]
    (is (some #(and (= (:op %) :db/retract)
                    (= (:e %) 1)
                    (= (:a %) :name)
                    (= (:v %) "Alice")) td))))

(deftest store-tx-data-multiple
  ;; Every fact in a multi-fact tx appears in tx-data, in order.
  (let [conn (store/open)
        r (store/transact conn [[:db/add 1 :name "Alice"]
                                [:db/add 1 :age 30]
                                [:db/add 2 :name "Bob"]])
        td (:tx-data r)]
    (is (= 3 (count td)))
    (is (= [1 1 2] (map :e td)))
    (is (= [:name :age :name] (map :a td)))))

(deftest store-tx-data-retract-entity
  ;; retractEntity produces one :db/retract tx-data entry per retracted fact.
  (let [conn (store/open)
        _ (store/transact conn {1 {:name "Alice" :age 30}})
        r (store/transact conn [:db/retractEntity 1])
        td (:tx-data r)
        retracted (filter #(= (:op %) :db/retract) td)]
    (is (= 2 (count retracted)) "two facts retracted")
    (is (= #{:name :age} (set (map :a retracted))))))

;; ---------------------------------------------------------------------------
;; Unique :value
;;
;; Spec-first. {:unique :value} enforces uniqueness (throws on duplicate)
;; but does NOT upsert -- the duplicate tx is rejected, the eid is never
;; rewritten.
;; ---------------------------------------------------------------------------
(deftest store-unique-value-first-write-ok
  ;; First write of a :unique :value attribute succeeds.
  (let [conn (store/open nil {:schema {:email {:unique :value}}})
        _ (store/transact conn [:db/add 1 :email "a@x.com"])
        db (store/db conn)]
    (is (= "a@x.com" (store/read db 1 :email)))))

(deftest store-unique-value-rejects-duplicate
  ;; A second entity carrying the same :unique :value throws.
  (let [conn (store/open nil {:schema {:email {:unique :value}}})
        _ (store/transact conn [:db/add 1 :email "a@x.com"])
        e (try
            (store/transact conn [:db/add 2 :email "a@x.com"])
            nil
            (catch e e))]
    (is (some? e) "duplicate :unique :value throws")
    (is (some? (re-find #"unique-conflict" (pr-str (ex-data e))))
        "ex-data carries ::unique-conflict tag")))

(deftest store-unique-value-no-upsert
  ;; Unlike :identity, :unique :value does NOT rewrite the eid; the
  ;; duplicate tx throws and leaves the existing entity alone.
  (let [conn (store/open nil {:schema {:email {:unique :value}}})
        _ (store/transact conn [:db/add 100 :email "a@x.com"])
        e (try
            (store/transact conn [:db/add 999 :email "a@x.com"])
            nil
            (catch e e))]
    (is (some? e) "duplicate throws rather than upserting")
    (is (= #{100} (store/entities (store/db conn)))
        "no new entity written; existing eid unchanged")))

;; ---------------------------------------------------------------------------
;; Pull :offset
;;
;; Spec-first. [:attr :offset M :limit N] skips M values then takes N;
;; [:attr :offset M] skips M and returns the rest.
;; ---------------------------------------------------------------------------
(deftest store-pull-offset
  ;; Skip 2, take next 2 of a :many attribute.
  (let [conn (store/open nil {:schema {:tags {:cardinality :many}}})
        _ (store/transact conn {1 {:tags #{:a :b :c :d :e}}})
        db (store/db conn)
        result (store/pull db [[:tags :offset 2 :limit 2]] 1)]
    (is (= 2 (count (:tags result))))
    (is (every? #{:a :b :c :d :e} (:tags result)))))

(deftest store-pull-offset-beyond
  ;; Offset past the available values yields an empty result for the attr.
  (let [conn (store/open nil {:schema {:tags {:cardinality :many}}})
        _ (store/transact conn {1 {:tags #{:a :b}}})
        db (store/db conn)
        result (store/pull db [[:tags :offset 10 :limit 5]] 1)]
    (is (empty? (:tags result)))))

(deftest store-pull-offset-without-limit
  ;; Offset alone skips M values and returns the rest.
  (let [conn (store/open nil {:schema {:tags {:cardinality :many}}})
        _ (store/transact conn {1 {:tags #{:a :b :c :d :e}}})
        db (store/db conn)
        result (store/pull db [[:tags :offset 2]] 1)]
    (is (= 3 (count (:tags result))) "skips 2, returns remaining 3")
    (is (every? #{:a :b :c :d :e} (:tags result)))))

;; ---------------------------------------------------------------------------
;; Component cascade cascade
;;
;; Spec-first. {:type :ref :isComponent true} marks a ref as owned; when
;; the parent is retractEntity'd, all component children are also
;; retracted. Non-component refs are nilled, not retracted.
;; ---------------------------------------------------------------------------
(deftest store-component-cascade-on-retract
  ;; retractEntity on the parent cascades to component children.
  (let [conn (store/open nil {:schema {:child {:type :ref :isComponent true}}})
        _ (store/transact conn [[:db/add 1 :name "Parent"]
                                [:db/add 2 :name "Child"]
                                [:db/add 1 :child 2]])
        _ (store/transact conn [:db/retractEntity 1])
        db (store/db conn)]
    (is (not (store/entity-exists? db 1)) "parent removed")
    (is (not (store/entity-exists? db 2)) "component child cascade-retracted")))

(deftest store-component-non-cascade-by-default
  ;; A plain (non-component) ref target survives the source's retraction.
  (let [conn (store/open nil {:schema {:child {:type :ref}}})
        _ (store/transact conn [[:db/add 1 :name "Parent"]
                                [:db/add 2 :name "Child"]
                                [:db/add 1 :child 2]])
        _ (store/transact conn [:db/retractEntity 1])
        db (store/db conn)]
    (is (store/entity-exists? db 2) "non-component child survives")))

(deftest store-component-nested
  ;; Cascade propagates through nested components: retracting the root
  ;; retracts the whole component tree.
  (let [conn (store/open nil {:schema {:child {:type :ref :isComponent true}}})
        _ (store/transact conn [[:db/add 1 :name "Root"]
                                [:db/add 2 :name "Mid"]
                                [:db/add 3 :name "Leaf"]
                                [:db/add 1 :child 2]
                                [:db/add 2 :child 3]])
        _ (store/transact conn [:db/retractEntity 1])
        db (store/db conn)]
    (is (not (store/entity-exists? db 1)))
    (is (not (store/entity-exists? db 2)))
    (is (not (store/entity-exists? db 3)) "nested component cascaded")))

;; ---------------------------------------------------------------------------
;; Change notification (store/listen)
;;
;; Spec-first. (store/listen conn key f) registers f to be called with
;; {:db-before :db-after :tx-data} on each transact; returns nil.
;; (store/unlisten conn key) removes the listener.
;; ---------------------------------------------------------------------------
(deftest store-listen-fires-on-transact
  ;; A registered listener is called once per transact.
  (let [conn (store/open)
        calls (atom 0)
        _ (store/listen conn :w (fn [_] (swap! calls inc)))
        _ (store/transact conn [:db/add 1 :name "Alice"])]
    (is (= 1 @calls))))

(deftest store-listen-receives-tx-data
  ;; The listener receives a map with :db-before, :db-after, and :tx-data.
  (let [conn (store/open)
        received (atom nil)
        _ (store/listen conn :w (fn [evt] (reset! received evt)))
        _ (store/transact conn [:db/add 1 :name "Alice"])]
    (is (map? @received))
    (is (contains? @received :db-before))
    (is (contains? @received :db-after))
    (is (contains? @received :tx-data))
    (is (sequential? (:tx-data @received)))))

(deftest store-listen-unlisten
  ;; An unlistened listener does not fire on later transactions.
  (let [conn (store/open)
        calls (atom 0)
        f (fn [_] (swap! calls inc))
        _ (store/listen conn :w f)
        _ (store/transact conn [:db/add 1 :name "Alice"])
        before @calls
        _ (store/unlisten conn :w)
        _ (store/transact conn [:db/add 2 :name "Bob"])]
    (is (= before @calls) "no further calls after unlisten")))

(deftest store-listen-multiple
  ;; Multiple distinct listeners all fire on each transact.
  (let [conn (store/open)
        a (atom 0) b (atom 0)
        _ (store/listen conn :a (fn [_] (swap! a inc)))
        _ (store/listen conn :b (fn [_] (swap! b inc)))
        _ (store/transact conn [:db/add 1 :name "Alice"])]
    (is (= 1 @a))
    (is (= 1 @b))))

(deftest store-listen-cleared-on-close
  ;; The listener registry is a process-global atom keyed by conn. Closing
  ;; the store must remove the conn's entry so the registry does not leak
  ;; the conn (and its closures) across many open/close cycles, and so a
  ;; listener registered before close is not retained.
  (let [conn (store/open)
        _    (store/listen conn :w (fn [_] nil))]
    (store/close conn)
    (is (nil? (get @@#'mino.store/listener-registry conn))
        "registry no longer holds the closed conn")))

(deftest store-migrate-coerce-accepts-symbol
  ;; migrate's :coerce docstring names the value as a "coerce-fn-sym".
  ;; A symbol must be resolved and applied, not invoked as a function
  ;; (symbols are not callable). Verify the documented form works.
  (let [conn (store/open nil {:schema {:n {:type :long}}})
        _    (store/put conn 1 :n 1)
        r    (store/migrate conn {:n {:type :long}}
                            {:coerce {:n 'inc}})]
    (is (= {1 {:n 2}} (:entities (:db-after r))))
    (store/close conn)))

;; ---------------------------------------------------------------------------
;; Disjunction
;;
;; Spec-first. (or clause...) unions the bindings of each alternative
;; branch, preserving shared variable bindings.
;; ---------------------------------------------------------------------------
(deftest store-q-or-basic
  ;; or of two patterns unions the result sets.
  (let [conn (store/open)
        _ (store/transact conn {1 {:name "Alice"}
                                2 {:label "Bob"}})
        db (store/db conn)]
    (is (= #{[1] [2]}
           (store/q db '[:find ?e
                         :where (or [?e :name ?n]
                                    [?e :label ?n])])))))

(deftest store-q-or-with-shared-var
  ;; or preserves shared variable bindings across branches; ?n is bound
  ;; by whichever branch matches.
  (let [conn (store/open)
        _ (store/transact conn {1 {:name "Alice" :age 30}
                                2 {:label "Bob"   :age 25}})
        db (store/db conn)]
    (is (= #{[1 "Alice"] [2 "Bob"]}
           (store/q db '[:find ?e ?n
                         :where [?e :age ?a]
                                (or [?e :name ?n]
                                    [?e :label ?n])])))))

;; ---------------------------------------------------------------------------
;; Raw datom access
;;
;; Spec-first. (store/datoms db index) returns a seq of {:e :a :v :tx}
;; maps in the named index order: :eavt, :avet, or :aevt.
;; ---------------------------------------------------------------------------
(deftest store-datoms-eavt
  ;; :eavt returns all datoms sorted by entity then attribute.
  (let [conn (store/open)
        _ (store/transact conn {1 {:name "Alice" :age 30}
                                2 {:name "Bob"}})
        db (store/db conn)
        ds (store/datoms db :eavt)]
    (is (sequential? ds))
    (is (= 3 (count ds)))
    (is (= [1 1 2] (map :e ds)))
    (is (= [:age :name :name] (map :a ds))
        "attrs sorted within each entity")))

(deftest store-datoms-avet
  ;; :avet sorts by attribute then value.
  (let [conn (store/open)
        _ (store/transact conn {1 {:age 30 :name "Alice"}
                                2 {:age 25 :name "Bob"}})
        db (store/db conn)
        ds (store/datoms db :avet)]
    (is (sequential? ds))
    (is (= [:age :age :name :name] (map :a ds))
        "grouped by attribute first")))

(deftest store-datoms-aevt
  ;; :aevt sorts by attribute then entity.
  (let [conn (store/open)
        _ (store/transact conn {1 {:age 30 :name "Alice"}
                                2 {:age 25 :name "Bob"}})
        db (store/db conn)
        ds (store/datoms db :aevt)]
    (is (sequential? ds))
    (is (= [:age :age :name :name] (map :a ds)))))

(deftest store-datoms-count
  ;; Total datom count equals the sum of attribute counts across entities.
  (let [conn (store/open)
        _ (store/transact conn {1 {:name "Alice" :age 30}
                                2 {:name "Bob" :email "b@x.com"}})
        db (store/db conn)]
    (is (= 4 (count (store/datoms db :eavt))))))

(deftest store-print-id-is-stable-counter-not-heap-address
  ;; #store[ID VAL] used to derive ID from (uintptr_t)v: it leaked the
  ;; heap cell address (info disclosure) and made prints nondeterministic
  ;; across runs. The ID is now a monotonic per-state counter (mirrors
  ;; agent/ref IDs), printed as 0xN in hex. A counter is 1-4 hex digits
  ;; (<= 65535 across a run); a leaked 64-bit heap address is 12+ hex
  ;; digits (e.g. 0x7f8b3c404000). So assert: two stores print
  ;; differently, and each id is short (counter-shaped, not pointer-shaped).
  (let [a (store/open)
        b (store/open)
        sa (pr-str a)
        sb (pr-str b)
        id-len (fn [s]
                 (let [m (re-find #"\#store\[0x([0-9a-f]+)" s)]
                   (if m (count (second m)) 0)))]
    (is (not= sa sb) "two stores print distinct ids")
    (is (<= 1 (id-len sa) 4) "id is a small counter, not a heap address")
    (is (<= 1 (id-len sb) 4))
    (store/close a)
    (store/close b)))

(deftest listen-fires-on-transact-not-on-compact-or-migrate
  ;; The listen contract is transact-scoped by design (see the `listen`
  ;; docstring): compact and migrate are maintenance/schema ops that
  ;; bypass the tx log and publish directly via store-commit*, so they
  ;; do NOT fire listeners. This pins the documented scope so a future
  ;; change to the contract breaks here loudly. Wiring listeners into
  ;; migrate/compact was considered and deferred: it would change
  ;; observed behavior for every existing listener caller and require
  ;; inventing an event shape for ops that have no natural :tx-data.
  (let [conn (store/open nil {:schema {:name {:type :any}}})
        fired (atom [])
        key :round2-scope-watch]
    (store/listen conn key #(swap! fired conj %))
    (store/transact conn {1 {:name "a"}})
    (store/compact conn)
    (store/migrate conn {:name {:type :string}})
    (is (= 1 (count @fired))
        "listener fired exactly once (transact only, not compact/migrate)")
    (store/close conn)))

(run-tests-and-exit)
