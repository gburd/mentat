(require "tests/test")
(require '[mino.store :as sstore])
(require '[clojure.string :as str])
(require '[clojure.test.check :as tc])
(require '[clojure.test.check.properties :as prop])
(require '[clojure.test.check.generators :as gen])

;; mino.store backend seam (ADR 35), third-party side. A backend that
;; is neither :memory nor :file rides the whole lifecycle through the
;; seam: open registers it, transact appends before publish through its
;; :commit, checkpoint and close own their own bytes, reopen replays
;; its segments with the db logic staying in mino.store. Malformed
;; custom backends are rejected with classified errors.

(def backend-test-dir "/tmp/mino-store-backend-test")

(defn atom-byte-store
  "A third-party byte store: a snapshot slot plus a WAL vector, both
  plain data."
  []
  (atom {:snapshot nil :wal [] :db-at-append nil}))

(defn atom-backend
  "A backend over an atom byte store, tagged with a fresh :kind. Each
  op touches only its own bytes: :commit appends the tx before the
  publish (recording the still-unpublished conn as the ordering
  witness), :checkpoint stores the current db and clears the WAL,
  :close final-checkpoints then releases the handle. The publish and
  the handle release ride the same prims every backend rides."
  [store]
  {:kind :atom-store
   :initial (fn [] (:snapshot @store))
   :wal-entries (fn [] (let [w (:wal @store)] (when (seq w) w)))
   :commit (fn [conn new-db tx-info]
             (swap! store assoc :db-at-append @conn)
             (swap! store update :wal conj tx-info)
             (store-commit* conn new-db tx-info))
   :checkpoint (fn [conn]
                 (swap! store assoc :snapshot @conn :wal [])
                 nil)
   :close (fn [conn]
            (swap! store assoc :snapshot @conn :wal [])
            (store-close* conn))})

(deftest custom-backend-round-trips-through-the-seam
  (let [store (atom-byte-store)
        backend (atom-backend store)
        conn (sstore/open nil {:backend backend})]
    (is (= backend (sstore/backend-for conn))
        "the prebuilt third-party map registers as-is")
    (is (= :atom-store (:kind (sstore/backend-for conn)))
        "neither built-in kind")
    (sstore/transact conn {1 {:name "Alice" :age 30}})
    (is (= 0 (:tx (:db-at-append @store)))
        "the backend saw the conn before the publish: append first")
    (is (= 1 (:tx (sstore/db conn))) "the publish took effect")
    ;; Crash without checkpoint or close: only the WAL survives.
    (sstore/dissoc-on-close conn)
    (let [conn2 (sstore/open nil {:backend backend})
          db2 (sstore/db conn2)]
      (is (= "Alice" (sstore/read db2 1 :name)) "WAL replay recovered the tx")
      (is (= 30 (sstore/read db2 1 :age)))
      (is (= 1 (:tx db2)) "the tx counter survived the replay")
      (sstore/checkpoint conn2)
      (is (= db2 (:snapshot @store)) "checkpoint stored the db value")
      (is (= [] (:wal @store)) "checkpoint cleared the WAL")
      (sstore/transact conn2 {2 {:name "Bob"}})
      (is (= 1 (count (:wal @store))) "a fresh WAL started after checkpoint")
      (sstore/close conn2)
      (is (nil? (sstore/backend-for conn2)) "close deregistered the backend")
      (is (= 2 (:tx (:snapshot @store))) "close final-checkpointed")
      (let [conn3 (sstore/open nil {:backend backend})
            db3 (sstore/db conn3)]
        (is (= "Alice" (sstore/read db3 1 :name)) "from the snapshot segment")
        (is (= "Bob" (sstore/read db3 2 :name))
            "close's final checkpoint captured the later tx")
        (is (= 2 (:tx db3)))
        (sstore/close conn3)))))

(deftest custom-backend-keeps-db-logic-in-the-store
  (let [store (atom-byte-store)
        backend (atom-backend store)
        conn (sstore/open nil {:backend backend
                               :schema {:name {:type :string}}
                               :closed true
                               :history {:keep-last 1}})]
    (let [e (try
              (sstore/transact conn {1 {:nickname "A"}})
              nil
              (catch Throwable e e))]
      (is (some? e) "an attribute outside the closed schema is rejected")
      (is (= [] (:wal @store))
          "the rejected tx never reached the backend's append"))
    (sstore/transact conn {1 {:name "A"} 2 {:name "B"}})
    (sstore/transact conn {1 {:name "A2"} 2 {:name "B2"}})
    (let [live (sstore/db conn)]
      (is (= 1 (count (:log live)))
          "the history policy compacted the log inside mino.store")
      (is (= "A2" (sstore/read live 1 :name)) "the entities view survived")
      (sstore/checkpoint conn)
      (sstore/close conn)
      (let [conn2 (sstore/open nil {:backend backend})]
        (is (= live (sstore/db conn2))
            "schema, history, and the compacted log rode the db value")
        (let [e (try
                  (sstore/transact conn2 {3 {:nickname "C"}})
                  nil
                  (catch Throwable e e))]
          (is (some? e) "the closed schema survived the reopen"))
        (sstore/close conn2)))))

(deftest open-rejects-malformed-custom-backends
  ;; A fresh :kind keyword is valid (that acceptance is the third-party
  ;; proof above); what throws is a structurally broken map.
  (let [good (atom-backend (atom-byte-store))
        rejected (fn [backend]
                   (try
                     (sstore/open nil {:backend backend})
                     nil
                     (catch Throwable e e)))
        e1 (rejected (dissoc good :commit))
        e2 (rejected (assoc good :kind "atom-store"))
        e3 (rejected 42)]
    (is (true? (sstore/backend? good))
        "the well-formed custom backend passes validation")
    (is (every? some? [e1 e2 e3]) "each malformed shape throws")
    (doseq [e [e1 e2 e3]]
      (is (= :store/backend (:mino/kind e))
          ":mino/kind classes each as :store/backend"))
    (is (= :commit (:op (ex-data e1)))
        "the missing-op error names the op")
    (is (= :caught
           (try (sstore/open nil {:backend (dissoc good :commit)})
                (catch :store/backend _ :caught)))
        "classed catch dispatches on :store/backend")))

;; ---------------------------------------------------------------------------
;; Byte identity across the seam (ADR 11 through ADR 35)
;; ---------------------------------------------------------------------------

(defn- direct-prim-transact
  "The pre-seam transact, reconstructed: the pure apply, then the
  direct store-commit* call carrying the same tx-info literal key
  order, bypassing the backend seam."
  [conn tx-data]
  (let [cur @conn
        tx-num (:tx cur)
        instant (store-clock* conn)
        new-db (:db-after (sstore/with cur tx-data))
        tx-info {:tx tx-num :instant instant :tx-data tx-data}]
    (store-commit* conn new-db tx-info)))

(defn- clock-normalized
  "Replaces every wall-clock :instant value with a fixed token; the
  clock digits are the only bytes that differ between two runs."
  [s]
  (str/replace s #":instant [0-9]+" ":instant CLOCK"))

(defn- wal-lines
  "The non-empty lines of an already-slurped WAL string."
  [wal]
  (vec (filter seq (str/split wal #"\n"))))

(deftest wal-and-snapshot-bytes-match-the-direct-prim-path
  ;; The same sequence runs twice: once through the pre-seam direct
  ;; prims, once through the seam. The WAL and snapshot files must be
  ;; byte-identical modulo the wall clock, and pinned exactly: the
  ;; golden line format, the 0x00 snapshot header, the printed-form
  ;; round trip, and equality with the live db.
  (rm-rf backend-test-dir)
  (mkdir-p backend-test-dir)
  (let [path-a (str backend-test-dir "/direct.db")
        path-b (str backend-test-dir "/seam.db")]
    (try
      (let [conn-a (sstore/open path-a)
            _ (direct-prim-transact conn-a {1 {:name "Alice" :age 30}})
            _ (direct-prim-transact conn-a [[:db/add 2 :name "Bob"]])
            wal-a (slurp (str path-a ".wal"))
            live-a (sstore/db conn-a)
            _ (do (store-checkpoint* conn-a)
                  (store-close* conn-a)
                  (sstore/dissoc-on-close conn-a))
            snap-a (slurp path-a)
            conn-b (sstore/open path-b)
            _ (sstore/transact conn-b {1 {:name "Alice" :age 30}})
            _ (sstore/transact conn-b [[:db/add 2 :name "Bob"]])
            wal-b (slurp (str path-b ".wal"))
            live-b (sstore/db conn-b)
            _ (sstore/checkpoint conn-b)
            _ (sstore/close conn-b)
            snap-b (slurp path-b)
            lines-a (wal-lines wal-a)
            lines-b (wal-lines wal-b)]
        (is (= (map clock-normalized lines-a)
               (map clock-normalized lines-b))
            "WAL lines identical on both sides of the seam")
        (is (= ["{:tx 0, :instant CLOCK, :tx-data {1 {:name \"Alice\", :age 30}}}"
                "{:tx 1, :instant CLOCK, :tx-data [[:db/add 2 :name \"Bob\"]]}"]
               (map clock-normalized lines-b))
            "the golden line format is pinned")
        (is (= 10 (int (get wal-b (dec (count wal-b)))))
            "the WAL ends with a newline")
        (is (= 2 (count lines-b)) "one line per transact")
        (doseq [l lines-b]
          (is (= l (pr-str (read-string l)))
              "each WAL line is exactly the printed tx-info"))
        (is (= [{:tx 0 :tx-data {1 {:name "Alice" :age 30}}}
                {:tx 1 :tx-data [[:db/add 2 :name "Bob"]]}]
               (map #(dissoc % :instant) (map read-string lines-b)))
            "the parsed entries carry the original tx-data")
        (let [ins (map :instant (map read-string lines-b))]
          (is (every? #(and (integer? %) (pos? %)) ins)
              "the clock values are positive integers")
          (is (<= (first ins) (second ins))
              "the clock does not go backwards across txs"))
        (is (= 0 (int (get snap-a 0))) "the direct path wrote the 0x00 header")
        (is (= 0 (int (get snap-b 0))) "the seam path wrote the 0x00 header")
        (is (= live-a (read-string (subs snap-a 1)))
            "the direct snapshot is exactly the live db")
        (is (= live-b (read-string (subs snap-b 1)))
            "the seam snapshot is exactly the live db")
        (is (= (subs snap-b 1) (pr-str (read-string (subs snap-b 1))))
            "the snapshot body is exactly the printed db value")
        (is (= (clock-normalized (subs snap-a 1))
               (clock-normalized (subs snap-b 1)))
            "snapshot bytes identical on both sides of the seam")
        (is (not (file-exists? (str path-a ".wal")))
            "checkpoint deleted the direct-path WAL")
        (is (not (file-exists? (str path-b ".wal")))
            "checkpoint deleted the seam-path WAL"))
      (finally
        (rm-rf backend-test-dir)))))

;; ---------------------------------------------------------------------------
;; Reopen-cycle property
;; ---------------------------------------------------------------------------

(def gen-fact
  (gen/tuple (gen/return :db/add)
             (gen/choose 1 4)
             (gen/elements [:name :age :score])
             (gen/elements ["alpha" "beta" "gamma" 7 42])))

(defn- reopen-cycle-preserves-db
  "Transacts the first half of batches, checkpoints, transacts the
  rest, closes, and reopens: the reopened db must equal the live db
  captured before the close, and the WAL must be gone."
  [batches]
  (rm-rf backend-test-dir)
  (mkdir-p backend-test-dir)
  (let [path (str backend-test-dir "/cycle.db")
        conn (sstore/open path)
        half (quot (count batches) 2)]
    (doseq [b (take half batches)]
      (sstore/transact conn (vec b)))
    (sstore/checkpoint conn)
    (doseq [b (drop half batches)]
      (sstore/transact conn (vec b)))
    (let [live (sstore/db conn)]
      (sstore/close conn)
      (let [reopened (sstore/open path)
            ok (and (= live (sstore/db reopened))
                    (not (file-exists? (str path ".wal"))))]
        (sstore/close reopened)
        (rm-rf backend-test-dir)
        ok))))

(deftest reopen-cycle-preserves-the-live-db
  (is (true? (:result (tc/quick-check
                        25
                        (prop/for-all [batches (gen/vector
                                                 (gen/vector gen-fact 1 3)
                                                 1 5)]
                          (reopen-cycle-preserves-db batches))
                        :seed 20260829)))
      "transact, checkpoint, transact, close, reopen equals the live db"))

(run-tests-and-exit)
