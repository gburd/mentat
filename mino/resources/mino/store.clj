;; mino.store — EAVT fact store for embedded per-runtime memory.
;;
;; Each store holds an accumulating log of facts [e a v tx instant op]
;; and a materialized view (entity -> attribute -> value). Stores are
;; per-state isolated; cross-runtime transfer uses mino_clone.
;;
;; The db value is a persistent map:
;;   {:entities {}   ;; entity-id -> {attr -> value}
;;    :log []        ;; flat vector of fact maps {:e :a :v :tx :instant :op}
;;    :tx 0          ;; next transaction number (monotonic per-store)
;;    :schema {}      ;; attribute -> {:type :string :cardinality :one}
;;    :closed? false} ;; when true, reject attributes not in :schema
;;
;; In-memory by default. Durability uses snapshot + WAL (see ADR 11):
;; each transact on a durable store appends a tx-info map to the WAL at
;; <path>.wal; checkpoint writes the snapshot and deletes the WAL; open
;; reads the snapshot then replays the WAL.
;;
;; Schema validation is opt-in via the :schema option on open. With
;; :closed true, only declared attributes are accepted.

(ns mino.store
  "EAVT fact store for embedded per-runtime memory.

  Each store holds an accumulating log of facts [e a v tx instant op]
  and a materialized view (entity -> attribute -> value). Stores are
  per-state isolated; cross-runtime transfer uses mino_clone.

  In-memory by default; durability via snapshot-on-checkpoint.")

;; The clojure.core store? predicate is referred into this namespace at
;; load time. Capturing it under a private alias lets the public store?
;; below delegate to the primitive instead of recursing into itself,
;; mirroring the -lock-* capture pattern used in core.clj.
(def ^:private c-store? store?)

;; clojure.core/merge is shadowed by the public store/merge below.
;; Capture it so internal functions (join-bindings) can merge plain
;; maps without calling the db-value merge.
(def ^:private map-merge merge)

(require '[clojure.set :as set])

;; ---------------------------------------------------------------------------
;; Data shape helpers
;; ---------------------------------------------------------------------------

(defn- empty-db
  "Returns a fresh db value with no entities, an empty log, a zero
  transaction counter, and an empty open schema."
  ([] {:entities {} :log [] :tx 0 :schema {} :closed? false})
  ([schema] {:entities {} :log [] :tx 0 :schema (or schema {}) :closed? false})
  ([schema closed?]
   {:entities {} :log [] :tx 0 :schema (or schema {}) :closed? (or closed? false)}))

(defn- make-fact
  "Builds a single fact map from its EAVT coordinates plus the tx number,
  a wall-clock instant, and the operation (:db/add or :db/retract)."
  [e a v tx instant op]
  {:e e :a a :v v :tx tx :instant instant :op op})

;; ---------------------------------------------------------------------------
;; Schema validation
;; ---------------------------------------------------------------------------

(def ^:private long-min -9223372036854775808)
(def ^:private long-max 9223372036854775807)

(defn- check-type
  "Returns nil if v matches type-spec, throws ex-info otherwise.
  Supported types: :string, :keyword, :long, :double, :boolean,
  :instant (treated as :long, 64-bit signed), :any (no check).
  Unknown type-spec values throw -- they are treated as schema
  definition errors so typos surface rather than silently disabling
  validation.

  For :cardinality :many attributes the value may be a set of
  members; each member is checked against type-spec individually,
  mirroring validate-preds' iteration over set values."
  ([attr v type-spec] (check-type attr v type-spec nil))
  ([attr v type-spec cardinality]
   (let [many?    (= :many cardinality)
         to-check (if (and many? (set? v)) v [v])]
     (doseq [val to-check]
       (let [ok? (case type-spec
                   :string  (string? val)
                   :keyword (keyword? val)
                   :long    (and (integer? val)
                                 (<= long-min val long-max))
                   :double  (float? val)
                   :boolean (boolean? val)
                   :instant (and (integer? val)
                                 (<= long-min val long-max))
                   :ref     true
                   :any     true
                   ;; Unknown type keyword: surface as a schema error instead
                   ;; of silently passing every value.
                   (throw
                     (ex-info (str "Unknown schema type spec for attribute " attr
                                   ": " type-spec)
                              {:attribute attr :type-spec type-spec})))]
         (when-not ok?
           (throw
             (ex-info (str "Type mismatch for attribute " attr
                           ": expected " type-spec)
                      {:attribute attr :value val :expected type-spec}))))))))

(defn- eid-ok?
  "Returns true if e is a valid entity id: a positive integer (>= 1,
  matching Datomic's address space and reserving negatives for future
  tempid support) or a keyword (natural-key style; lookup-refs are
  resolved before this check, so unresolved vectors fail here)."
  [e]
  (cond
    (integer? e) (pos? e)
    (keyword? e) true
    :else false))

(defn- attr-ok?
  "Returns true if a is a valid attribute key: a non-empty keyword
  (the Datomic convention) or a non-empty string (some callers use
  string keys). nil is handled by the caller; empty keys are always
  a caller bug and surface here."
  [a]
  (cond
    (keyword? a) (not= (keyword "") a)
    (string? a)  (not= "" a)
    :else false))

(defn- validate-fact
  "Validates a single fact against the schema. Throws ex-info on
  violation: nil entity-id, nil attribute, unknown attribute in a
  closed schema, or type mismatch on :db/add. No-op when schema is
  empty. nil as a fact value on :db/add is a documented v1 design
  choice (see store-add-nil-value) and is allowed."
  [schema closed? {:keys [e a v op]}]
  (when (nil? e)
    (throw
      (ex-info "nil entity id is not allowed"
               {:attribute a})))
  (when (not (eid-ok? e))
    (throw
      (ex-info (str "Invalid entity id (must be a positive integer or a keyword): "
                    (pr-str e))
               {:entity e :attribute a})))
  (when (nil? a)
    (throw
      (ex-info "nil attribute is not allowed"
               {:entity e})))
  (when (not (attr-ok? a))
    (throw
      (ex-info (str "Invalid attribute (must be a non-empty keyword or string): "
                    (pr-str a))
               {:entity e :attribute a})))
  (let [spec (get schema a)]
    (when (and closed? (not spec))
      (throw
        (ex-info (str "Unknown attribute in closed schema: " a)
                 {:attribute a :entity e})))
    (when (and spec (:type spec) (not= (:type spec) :any)
               (= op :db/add))
      (check-type a v (:type spec) (:cardinality spec)))))

(defn- type-matches?
  "Non-throwing type check. Returns true if v matches type-spec."
  [v type-spec]
  (case type-spec
    :string  (string? v)
    :keyword (keyword? v)
    :long    (and (integer? v) (<= long-min v long-max))
    :double  (float? v)
    :boolean (boolean? v)
    :instant (and (integer? v) (<= long-min v long-max))
    :ref     true
    :any     true
    true))

(defn- coerce-fn
  "Normalizes a :coerce spec value into a callable function. Accepts
  either a function value (passed directly) or a symbol (resolved in
  the calling namespace, mirroring how migrate's docstring names the
  value as a 'coerce-fn-sym'). Symbols are not callable in mino; an
  unresolved symbol throws ::invalid-coerce so the failure surfaces
  at migration time rather than silently producing nil."
  [cf]
  (cond
    (fn? cf) cf
    (symbol? cf)
    (let [v (resolve cf)]
      (when-not (and v (fn? @v))
        (throw (ex-info (str "migrate :coerce symbol not resolvable to a function: " cf)
                        {::invalid-coerce cf})))
      @v)
    :else (throw (ex-info (str "migrate :coerce spec must be a function or symbol: " cf)
                          {::invalid-coerce cf}))))

(defn- validate-preds
  "Validates attribute predicates on a fact. Each pred symbol is
  resolved and called with the value. For :many cardinality with a
  set value, each member is checked. Falsy return throws
  ::attr-pred-failed."
  [schema {:keys [e a v op]}]
  (when (= op :db/add)
    (let [spec (get schema a)
          preds (or (:preds spec) [])
          many? (= :many (:cardinality spec))
          vals (if (and many? (set? v)) v [v])]
      (doseq [pred preds
              val vals]
        (when-not (pred val)
            (throw
              (ex-info (str "Attribute predicate failed for " a)
                       {::attr-pred-failed {:attr a :value val}})))))))

;; ---------------------------------------------------------------------------
;; Index maintenance
;; ---------------------------------------------------------------------------

(defn- build-indexes
  "Builds the index map from entities for the given set of indexed
  attributes. Returns a map: attr -> {value -> #{entity-ids}}.
  Handles set-valued (:many) attributes by indexing each member."
  [entities indexed-attrs]
  (if (empty? indexed-attrs)
    {}
    (reduce (fn [idx [e attrs]]
              (reduce (fn [idx a]
                        (if (contains? indexed-attrs a)
                          (let [v   (get attrs a)
                                vals (cond
                                       (set? v) v
                                       (nil? v) nil
                                       :else #{v})]
                            (if vals
                              (reduce (fn [idx val]
                                        (let [cur (get-in idx [a val] #{})]
                                          (assoc-in idx [a val] (conj cur e))))
                                      idx vals)
                              idx))
                          idx))
                      idx (keys attrs)))
            {} entities)))

;; ---------------------------------------------------------------------------
;; Retention
;; ---------------------------------------------------------------------------

(defn- maybe-compact-log
  "Checks the :history policy on db and compacts the log if it exceeds
  the threshold. The materialized entities view is always preserved;
  only old log entries are dropped. Returns db unchanged when no
  policy is active or the threshold is not exceeded."
  [db]
  (let [history (get db :history)
        log (:log db)]
    (cond
      (nil? history) db

      (and (:keep-last history)
           (> (count log) (* 2 (:keep-last history))))
      (let [kept (take-last (:keep-last history) log)]
        (assoc db :log (vec kept)))

      (and (:keep-since history)
           (some #(< (:instant % 0) (:keep-since history)) log))
      (let [cutoff (:keep-since history)
            kept (filter #(>= (:instant % 0) cutoff) log)]
        (assoc db :log (vec kept)))

      :else db)))

;; ---------------------------------------------------------------------------
;; Schema validation for open-time errors
;; ---------------------------------------------------------------------------

(defn- validate-schema
  "Validates a schema map at open time. Checks that :unique is only
  declared on :cardinality :one attributes."
  [schema]
  (doseq [[attr spec] schema]
    (when (and (:unique spec)
               (= :many (:cardinality spec)))
      (throw
        (ex-info (str "Attribute " attr " cannot be :unique"
                      " with :cardinality :many")
                 {:attribute attr :spec spec}))))
  schema)

(defn- unique-attrs
  "Returns the set of attribute keywords with any :unique declaration."
  [schema]
  (set (for [[a spec] schema
             :when (:unique spec)]
         a)))

(defn- identity-attrs
  "Returns attrs with :unique :identity (upsert)."
  [schema]
  (set (for [[a spec] schema
             :when (= :identity (:unique spec))]
         a)))

(defn- ref-attrs
  "Returns the set of attribute keywords declared :type :ref."
  [schema]
  (set (for [[a spec] schema
             :when (= :ref (:type spec))]
         a)))

;; ---------------------------------------------------------------------------
;; Lookup-ref and upsert resolution
;; ---------------------------------------------------------------------------

(defn- lookup-ref?
  "Returns true if x is a 2-element vector [keyword value] that could
  be a lookup-ref."
  [x]
  (and (vector? x) (= (count x) 2) (keyword? (first x))))

(defn- resolve-lookup-refs
  "Resolves lookup-refs in the entity-id position of parsed ops.
  A lookup-ref [:unique-attr value] is resolved via the unique index."
  [db ops]
  (let [schema (:schema db)
        indexes (:indexes db)
        u-attrs (unique-attrs schema)]
    (for [[op e a v] ops]
      (if (and (lookup-ref? e) (contains? u-attrs (first e)))
        (let [[ref-attr ref-val] e
              existing (seq (get-in indexes [ref-attr ref-val]))]
          (if existing
            [op (first existing) a v]
            (throw (ex-info (str "Lookup-ref " e " does not resolve")
                            {:lookup-ref e}))))
        [op e a v]))))

(defn- resolve-upserts
  "Rewrites entity-ids for upsert (:unique :identity) and detects
  conflicts for both :identity and :value unique attrs."
  [db ops]
  (let [schema (:schema db)
        indexes (:indexes db)
        id-attrs (identity-attrs schema)]
    (if (empty? (unique-attrs schema))
      ops
      (loop [remaining ops
             eid-map {}
             tx-uniques {}]
        (if-let [[op e a v] (first remaining)]
          (if (and (= op :db/add) (contains? (unique-attrs schema) a))
            (let [u-type (get-in schema [a :unique])
                  pre-existing (seq (get-in indexes [a v]))
                  tx-existing (get tx-uniques [a v])
                  mapped-e (get eid-map e e)]
              (cond
                ;; Two tx claims on same unique value, different eids
                (and tx-existing (not= tx-existing mapped-e))
                (throw (ex-info (str "Unique conflict on " a ": " v)
                                {::unique-conflict {:attr a :value v}}))

                ;; Pre-existing + :value + different eid = conflict (no upsert)
                (and pre-existing (= u-type :value)
                     (not= (first pre-existing) mapped-e))
                (throw (ex-info (str "Unique conflict on " a ": " v)
                                {::unique-conflict {:attr a :value v}}))

                ;; Pre-existing + :identity + different eid = upsert
                (and pre-existing (= u-type :identity)
                     (not= (first pre-existing) mapped-e))
                (recur (rest remaining)
                       (assoc eid-map e (first pre-existing))
                       (assoc tx-uniques [a v] (first pre-existing)))

                ;; Pre-existing, same eid = OK
                pre-existing
                (recur (rest remaining) eid-map
                       (assoc tx-uniques [a v] (first pre-existing)))

                ;; First time seeing this value
                :else
                (recur (rest remaining) eid-map
                       (assoc tx-uniques [a v] mapped-e))))
            (recur (rest remaining) eid-map tx-uniques))
          (for [[op e a v] ops]
            [op (get eid-map e e) a v]))))))

(defn- extract-ensures
  "Extracts {:db/ensure eid -> spec-name} from tx-data. Returns a map
  of ensures that were found. For map-form tx-data, :db/ensure is a
  virtual key; for other forms, returns empty map."
  [tx-data]
  (cond
    (map? tx-data)
    (into {} (for [[e attrs] tx-data
                   :when (:db/ensure attrs)]
               [e (:db/ensure attrs)]))
    (seq? tx-data)
    (reduce conj {} (map extract-ensures tx-data))
    (vector? tx-data)
    (if (or (= (first tx-data) :db/add)
            (= (first tx-data) :db/retract))
      {}
      (reduce conj {} (map extract-ensures tx-data)))
    :else {}))

(defn- no-history-attrs
  "Returns the set of attrs declared :no-history true."
  [schema]
  (set (for [[a spec] schema :when (:no-history spec)] a)))

(defn- validate-entity-specs
  "Validates entity specs on entities that had :db/ensure. Called after
  all facts are applied. Throws ::entity-spec-failed."
  [entities entity-specs ensures]
  (let [db-proxy {:entities entities}]
    (doseq [[eid spec-name] ensures]
      (let [spec (get entity-specs spec-name)
            entity (get entities eid)]
        (when spec
          (let [required (:required-attrs spec)
                missing (filter #(nil? (get entity %)) required)]
            (when (seq missing)
              (throw
                (ex-info (str "Entity spec " spec-name " failed for entity " eid
                              ": missing required attrs " (vec missing))
                         {::entity-spec-failed {:eid eid :spec spec-name
                                                :missing-attrs (vec missing)}}))))
          (doseq [pred (:preds spec)]
            (when-not (pred db-proxy eid)
                (throw
                  (ex-info (str "Entity spec " spec-name
                                " predicate failed for entity " eid)
                           {::entity-spec-failed {:eid eid :spec spec-name}})))))))))

(defn- validate-refs
  "Validates that :db/add facts on :ref attrs target eids that exist
  in the pre-tx db or are created in this tx. Throws ::dangling-ref."
  [db ops]
  (let [schema (:schema db)
        r-attrs (ref-attrs schema)
        existing-eids (set (keys (:entities db)))
        tx-created-eids (set (for [[op e a v] ops :when (= op :db/add)] e))
        all-known (set/union existing-eids tx-created-eids)]
    (doseq [[op e a v] ops
            :when (and (= op :db/add) (contains? r-attrs a))]
      (let [vals (if (set? v) v #{v})]
        (doseq [val vals]
          (when-not (contains? all-known val)
            (throw
              (ex-info (str "Dangling ref: " a " = " val
                            " does not reference an existing entity")
                       {::dangling-ref {:attr a :value val :entity e}}))))))))

(defn- cleanup-dangling-refs
  "After applying facts, removes ref values that point at entities that
  no longer exist (retracted entities). Returns updated entities."
  [entities schema]
  (let [r-attrs (ref-attrs schema)
        existing (set (keys entities))]
    (if (empty? r-attrs)
      entities
      (into {}
        (for [[e attrs] entities]
          (let [cleaned
                (reduce (fn [m a]
                          (if (contains? r-attrs a)
                            (let [v (get m a)]
                              (cond
                                (nil? v) m
                                (set? v)
                                (let [keep (set/intersection v existing)]
                                  (if (empty? keep)
                                    (dissoc m a)
                                    (assoc m a keep)))
                                (contains? existing v) m
                                :else (dissoc m a)))
                            m))
                        attrs (keys attrs))]
            (when (seq cleaned) [e cleaned])))))))

;; ---------------------------------------------------------------------------
;; Reverse index
;; ---------------------------------------------------------------------------

(defn- build-reverse-index
  "Builds the reverse index {ref-attr {target-eid #{source-eid}}} from
  entities and schema. Called lazily by referring/referred-by."
  [entities schema]
  (let [r-attrs (ref-attrs schema)]
    (if (empty? r-attrs)
      {}
      (reduce (fn [idx [e attrs]]
                (reduce (fn [idx a]
                          (if (contains? r-attrs a)
                            (let [v (get attrs a)
                                  vals (cond (set? v) v
                                             (nil? v) nil
                                             :else #{v})]
                              (if vals
                                (reduce (fn [idx val]
                                          (let [cur (get-in idx [a val] #{})]
                                            (assoc-in idx [a val] (conj cur e))))
                                        idx vals)
                                idx))
                              idx))
                        idx (keys attrs)))
              {} entities))))

;; ---------------------------------------------------------------------------
;; Lifecycle
;; ---------------------------------------------------------------------------

(defn open
  "Opens a store connection. With no args, opens an in-memory store.
  With a path string, opens a durable store (reads snapshot + replays
  WAL if files exist). The options map may carry:
    :schema    map of attribute -> {:type :cardinality} spec
    :closed    when true, reject attributes not in :schema (default false)
    :indexes   set of attributes to maintain reverse indexes for
    :history   {:keep-last N} or {:keep-since T} for auto-compaction
  Schema, indexes, and history are set at first open and stored in the
  db value; on reopen the snapshot's values take precedence."
  ([] (store-open* (empty-db) nil))
  ([path] (open path nil))
  ([path opts]
   (let [schema (validate-schema (:schema opts))
         snap-db (when path (store-read-snapshot* path))
         base (or snap-db
                    (-> (empty-db schema (:closed opts))
                        (assoc :indexed-attrs (set/union (or (:indexes opts) #{})
                                                          (unique-attrs schema)))
                        (assoc :history (:history opts))
                        (assoc :entity-specs (:entity-specs opts))))
          entries (when path (store-read-wal* path))
          db (if (seq entries)
               (let [snapshot-tx (:tx base)]
                 (reduce (fn [d entry]
                           (if (< (:tx entry) snapshot-tx)
                             d
                             (dissoc (apply-tx d (:tx entry) (:instant entry)
                                       (:tx-data entry)) :tx-data)))
                         base entries))
               base)
          db (maybe-compact-log db)]
     (store-open* db path))))

(defn close
  "Flushes (if durable) and closes the store. Idempotent. Also clears
  any listeners registered on conn so the process-global listener
  registry does not retain the conn (or its listener closures) after
  close."
  [conn]
  (swap! listener-registry dissoc conn)
  (store-close* conn)
  nil)

(defn db
  "Returns the store's current immutable db value (a snapshot)."
  [conn]
  @conn)

(defn checkpoint
  "Writes the db value to disk if the store is durable. No-op for an
  in-memory store."
  [conn]
  (store-checkpoint* conn)
  nil)

(defn store?
  "Returns true if x is a store connection."
  [x]
  (c-store? x))

;; ---------------------------------------------------------------------------
;; Transaction parsing
;; ---------------------------------------------------------------------------

(defn- parse-tx-data
  "Parses tx-data into a flat seq of [op e a v] tuples. Accepts three
  forms:

    [:db/add e a v]       — assert
    [:db/retract e a v]   — retract a specific value
    [:db/retract e a]     — retract every value of attribute a
    {e {a v a2 v2 ...}}   — map sugar (all adds)

  A seq of any of the above is flattened recursively; an item that
  matches none of the shapes throws."
  [tx-data]
  (cond
    (and (vector? tx-data)
         (or (= (first tx-data) :db/add)
             (= (first tx-data) :db/retract)))
    (condp = (count tx-data)
      4 [[(tx-data 0) (tx-data 1) (tx-data 2) (tx-data 3)]]
      3 (if (= (first tx-data) :db/retract)
          [[:db/retract (tx-data 1) (tx-data 2) nil]]
          (throw (ex-info "[:db/add e a] is missing a value"
                          {:tx-data tx-data})))
      (throw (ex-info "tx-data vector has wrong arity (expected 3 or 4 elements)"
                      {:tx-data tx-data})))

    (map? tx-data)
    (vec (for [[e attrs] tx-data
               [a v] attrs
               :when (not= a :db/ensure)]
           [:db/add e a v]))

    (seq? tx-data)
    (mapcat parse-tx-data tx-data)

    (vector? tx-data)
    (mapcat parse-tx-data tx-data)

    :else
    (throw (ex-info "Invalid tx-data format" {:tx-data tx-data}))))

(defn- expand-retract-entity
  "Pre-processes tx-data to expand [:db/retractEntity eid] into
  individual [:db/retract eid attr] ops. Cascades to :isComponent
  ref children recursively. Returns a flat seq of tx-data items."
  [entities schema tx-data]
  (cond
    (and (vector? tx-data) (= (count tx-data) 2)
         (= (first tx-data) :db/retractEntity))
    (let [eid (second tx-data)
          attrs (get entities eid)]
      (if attrs
        (let [component-attrs (for [[a spec] schema :when (:isComponent spec)] a)
              retracts (for [a (keys attrs)] [:db/retract eid a])
              children (for [ca component-attrs
                             :when (contains? attrs ca)
                             :let [v (get attrs ca)]
                             child (if (set? v) v #{v})]
                         child)
              cascades (mapcat #(expand-retract-entity entities schema [:db/retractEntity %]) children)]
          (concat retracts cascades))
        '()))

    (and (vector? tx-data)
         (not (= (first tx-data) :db/add))
         (not (= (first tx-data) :db/retract)))
    (mapcat #(expand-retract-entity entities schema %) tx-data)

    (seq? tx-data)
    (mapcat #(expand-retract-entity entities schema %) tx-data)

    (map? tx-data)
    (list tx-data)

    :else (list tx-data)))

;; ---------------------------------------------------------------------------
;; Transaction application
;; ---------------------------------------------------------------------------

(defn- retract-attr
  "Removes attribute a from entity, dropping the entity entirely when
  no attributes remain. Returns the updated entities map."
  [entities e entity a]
  (let [entity' (dissoc entity a)]
    (if (empty? entity')
      (dissoc entities e)
      (assoc entities e entity'))))

(defn- apply-fact
  "Applies a single fact to the entities map, returning the updated
  entities. :db/add overwrites the attribute (single-valued) or conjs
  into a set (:many cardinality from schema). :db/retract with a nil
  value drops the whole attribute; with a concrete value it drops the
  attribute when the current value matches (single-valued) or disj's
  the value from a multi-valued set, removing the attribute when the
  set becomes empty.
  Performs no schema validation: apply-tx is the single validation
  boundary, so callers (replay, merge, as-of) must feed it facts that
  apply-tx has already validated."
  ([entities fact] (apply-fact entities fact nil))
  ([entities {:keys [e a v op]} schema]
   (let [entity  (or (get entities e) {})
         spec    (get schema a)
         many?   (= :many (:cardinality spec))]
     (case op
      :db/add
        (if many?
          (let [cur (get entity a)]
            (assoc entities e (assoc entity a (if (set? v)
                                                 (set/union (or cur #{}) v)
                                                 (conj (or cur #{}) v)))))
          (assoc entities e (assoc entity a v)))

       :db/retract
       (if (nil? v)
         (retract-attr entities e entity a)
         (let [cur (get entity a)]
           (cond
             (nil? cur) entities
             (set? cur)
             (if (contains? cur v)
               (let [cur' (disj cur v)]
                 (if (empty? cur')
                   (retract-attr entities e entity a)
                   (assoc entities e (assoc entity a cur'))))
               entities)
              (= cur v)
             (retract-attr entities e entity a)
             :else entities)))))))

(defn- apply-tx
  "Applies a parsed transaction to the db value, returning a new db
  value. Each op becomes a fact tagged with tx-num and instant; the
  materialized view is rebuilt by folding the facts, and the flat log
  grows by the new facts. The tx counter advances by one. Schema,
  closed?, indexes, and history policy are preserved from the input db.
  Each fact is validated against the schema before application."
  [db tx-num instant tx-data]
  (let [schema        (get db :schema {})
        closed?       (get db :closed? false)
        indexed-attrs (get db :indexed-attrs #{})
        history       (get db :history)
        entity-specs  (get db :entity-specs)
        expanded      (expand-retract-entity (:entities db) schema tx-data)
        ensures       (extract-ensures tx-data)
        ops           (->> (parse-tx-data expanded)
                           (resolve-lookup-refs db)
                           (resolve-upserts db))
        _             (validate-refs db ops)
        facts         (vec (for [[op e a v] ops]
                             (make-fact e a v tx-num instant op)))]
    (doseq [f facts]
      (validate-fact schema closed? f)
      (validate-preds schema f))
    (let [entities (reduce (fn [acc f] (apply-fact acc f schema))
                           (:entities db) facts)
          entities (cleanup-dangling-refs entities schema)
          _        (validate-entity-specs entities entity-specs ensures)
          indexes  (build-indexes entities indexed-attrs)
          nh-attrs (no-history-attrs schema)
          log-facts (if (empty? nh-attrs)
                      facts
                      (filter #(not (contains? nh-attrs (:a %))) facts))
          tx-facts (for [f facts] {:e (:e f) :a (:a f) :v (:v f) :op (:op f)})]
      {:entities entities
       :log (into (:log db) log-facts)
       :tx (inc tx-num)
       :schema schema
       :closed? closed?
       :indexed-attrs indexed-attrs
       :indexes indexes
       :entity-specs entity-specs
       :history history
       :tx-data tx-facts})))

;; ---------------------------------------------------------------------------
;; Public transaction API
;; ---------------------------------------------------------------------------

(def ^:private listener-registry (atom {}))

(defn listen
  "Registers f to be called with {:db-before :db-after :tx-data} on
  each transact. key is used for unregistration. Returns nil.

  Scope is the transaction stream only: `put`, `retract`, and
  `transact` (which they delegate to) all fire f. The maintenance and
  schema ops `compact` and `migrate` do NOT fire f -- they bypass the
  tx log and publish directly via store-commit*. If you need to react
  to a compact/migrate db change, poll `(db conn)` after the call.
  (The C-level `add-watch` does not cover stores either: stores are
  not in the watchable-get table, so store->watches is never populated.
  `listen`/`fire-listeners` is the only observer surface.)"
  [conn key f]
  (swap! listener-registry assoc-in [conn key] f)
  nil)

(defn unlisten
  "Removes the listener registered under key. Returns nil."
  [conn key]
  (swap! listener-registry update-in [conn] dissoc key)
  nil)

(defn- fire-listeners
  "Calls all registered listeners for conn with the event map."
  [conn event]
  (let [listeners (get @listener-registry conn)]
    (doseq [[_ f] listeners]
      (f event))))

(defn transact
  "Transacts facts against the store connection. Atomic: all-or-nothing.
  tx-data accepts any of the parse-tx-data forms, plus [:db/retractEntity eid].
  Returns {:tx N :db-after db :tx-data [...]}. On a durable store, the
  transaction is appended to the WAL before the in-memory publish."
  [conn tx-data]
  (let [cur @conn
        tx-num (:tx cur)
        instant (store-clock* conn)
        result (maybe-compact-log
                 (apply-tx cur tx-num instant tx-data))
        tx-facts (:tx-data result)
        new-db (dissoc result :tx-data)
        tx-info {:tx tx-num :instant instant :tx-data tx-data}]
    (store-commit* conn new-db tx-info)
    (fire-listeners conn {:db-before cur :db-after new-db :tx-data tx-facts})
    {:tx tx-num :db-after new-db :tx-data tx-facts}))

(defn with
  "Pure variant of transact: applies tx-data to a db value without
  touching a connection. Returns {:tx N :db-after new-db}. Useful for
  speculating over a snapshot."
  [db-val tx-data]
  (let [tx-num (:tx db-val)
        instant (store-clock* nil)
        result (apply-tx db-val tx-num instant tx-data)
        new-db (dissoc result :tx-data)]
    {:tx tx-num :db-after new-db :tx-data (:tx-data result)}))

(defn put
  "Asserts a single fact. Equivalent to
  (transact conn [[:db/add e a v]])."
  [conn e a v]
  (transact conn [[:db/add e a v]]))

(defn retract
  "Retracts a fact. With three args, retracts every value of attribute
  a on entity e. With four args, retracts only the specific value v."
  ([conn e a]
   (transact conn [[:db/retract e a]]))
  ([conn e a v]
   (transact conn [[:db/retract e a v]])))

;; ---------------------------------------------------------------------------
;; Point reads
;; ---------------------------------------------------------------------------

(defn read
  "Reads a single attribute value for an entity. Accepts either
  (read db-val e a) or (read db-val [e a]). Returns nil when the entity
  or attribute is absent."
  ([db-val e a]
   (get-in (:entities db-val) [e a]))
  ([db-val [e a]]
   (get-in (:entities db-val) [e a])))

(defn entity
  "Returns all attributes of entity e as a map, tagged with the entity
  id under :db/id. Returns nil when the entity is absent."
  [db-val e]
  (let [attrs (get (:entities db-val) e)]
    (when attrs (assoc attrs :db/id e))))

(defn entity-exists?
  "Returns true if entity e exists in the db value."
  [db-val e]
  (contains? (:entities db-val) e))

;; ---------------------------------------------------------------------------
;; Collection reads
;; ---------------------------------------------------------------------------

(defn entities
  "Returns the set of all entity ids in the db value."
  [db-val]
  (set (keys (:entities db-val))))

(defn where
  "Filters entities by pred, which receives each entity map (including
  :db/id). Returns a seq of the matching entity maps."
  [db-val pred]
  (filter pred
          (for [[e attrs] (:entities db-val)]
            (assoc attrs :db/id e))))

(defn find-by
  "Finds entity ids where attr equals value. Uses the reverse index if
  one is maintained for attr (via :indexes on open); otherwise falls
  back to a linear scan. Handles :many cardinality attributes by
  checking set membership. Returns the single id when exactly one entity
  matches, a set of ids when several match, and nil when none do."
  [db-val attr value]
  (if-let [index (get-in db-val [:indexes attr])]
    (let [found (get index value)]
      (cond
        (nil? found) nil
        (= (count found) 1) (first found)
        :else found))
    (let [found (for [[e attrs] (:entities db-val)
                      :let [cur (get attrs attr)]
                      :when (or (= cur value)
                                (and (set? cur) (contains? cur value)))]
                  e)]
      (cond
        (empty? found) nil
        (= (count found) 1) (first found)
        :else (set found)))))

;; ---------------------------------------------------------------------------
;; Temporal reads
;; ---------------------------------------------------------------------------

(defn- point-as-instant?
  "Returns true when point is a #inst value (or any value carrying
  the :mino/instant metadata marker). Used by as-of and since to
  dispatch the temporal axis by argument TYPE, matching Datomic's
  (d/as-of db inst) vs (d/as-of db t) convention: callers pass an
  inst for wall-clock semantics, anything else (typically a long)
  for tx-number semantics. Replaces an earlier magnitude heuristic
  that conflated tx and instant whenever the process's clock value
  fell below 1e9 (i.e. any process up < 12 days, or any custom
  clock returning small counts)."
  [point]
  (inst? point))

(defn- as-point-ms
  "Normalizes the as-of/since point argument to an epoch-ms long
  when it is an inst; returns other values unchanged. Callers then
  compare against :instant (epoch ms) for the inst branch or :tx
  for the tx branch.

  Validates point is either an inst or an integer; anything else
  throws ::invalid-point so callers see a clear argument error
  rather than a downstream numeric-comparator crash."
  [point]
  (cond
    (point-as-instant? point) (inst-ms point)
    (integer? point) point
    :else (throw
            (ex-info (str "as-of/since point must be an inst or an integer, got: "
                          (pr-str point))
                     {::invalid-point point}))))

(defn as-of
  "Returns the db value as it was at tx N or instant T. Replays the log
  up to (but not including) the point, rebuilding the materialized view
  in tx order. The returned :tx, :schema, :closed?, :indexed-attrs, and
  :history are preserved from the input. Indexes are rebuilt for the
  as-of entity view.

  WARNING: when the store has a :history {:keep-last N} or
  {:keep-since T} retention policy, as-of for points earlier than the
  retained window returns a PARTIAL view -- old facts have been dropped
  from the log. The materialized entities view reflects only retained
  facts replayed. Attributes declared :no-history return their current
  value at all points (they were never logged).

  The point argument is dispatched by type, matching Datomic: an inst
  (#inst \"...\" or any value satisfying inst?) selects wall-clock
  semantics; any other value (typically a long) selects tx-number
  semantics."
  [db-val point]
  (let [schema (get db-val :schema {})
        indexed-attrs (get db-val :indexed-attrs #{})
        instant? (point-as-instant? point)
        cmp     (as-point-ms point)
        facts-before (filter (fn [f]
                                (if instant?
                                  (< (:instant f) cmp)
                                  (< (:tx f) cmp)))
                              (:log db-val))
        ordered (sort-by :tx facts-before)
        entities (reduce (fn [acc f] (apply-fact acc f schema))
                         {} ordered)
        nh-attrs (no-history-attrs schema)
        entities (if (empty? nh-attrs)
                   entities
                   (reduce (fn [ents [e attrs]]
                             (let [nh-vals (into {} (for [a nh-attrs
                                                          :when (contains? attrs a)]
                                                      [a (get attrs a)]))]
                               (if (empty? nh-vals)
                                 ents
                                 (let [cur (or (get ents e) {})]
                                   (assoc ents e (conj cur nh-vals))))))
                           entities (:entities db-val)))
        indexes (build-indexes entities indexed-attrs)]
    {:entities entities :log (vec ordered) :tx (:tx db-val)
     :schema schema :closed? (get db-val :closed? false)
     :indexed-attrs indexed-attrs :indexes indexes
     :entity-specs (get db-val :entity-specs)
     :history (get db-val :history)}))

(defn since
  "Returns the seq of facts asserted at or after tx N (or instant T),
  in log order. The point argument is dispatched by type, matching
  Datomic: an inst selects wall-clock semantics; any other value
  (typically a long) selects tx-number semantics."
  [db-val point]
  (let [instant? (point-as-instant? point)
        cmp      (as-point-ms point)]
    (filter (fn [f]
              (if instant?
                (>= (:instant f) cmp)
                (>= (:tx f) cmp)))
            (:log db-val))))

(defn history
  "Returns the full history of an entity, optionally narrowed to one
  attribute, as a seq of facts in log order."
  ([db-val e]
   (filter #(= (:e %) e) (:log db-val)))
  ([db-val e a]
   (filter #(and (= (:e %) e) (= (:a %) a)) (:log db-val))))

;; ---------------------------------------------------------------------------
;; Recency queries
;; ---------------------------------------------------------------------------

(defn recent
  "Returns the last N facts in temporal order, most recent first.
  The optional opts map may carry:
    :where      a predicate over a fact
    :entities   a set of entity ids to scope to
    :attributes a set of attribute keys to scope to"
  ([db-val n]
   (take n (reverse (:log db-val))))
  ([db-val n opts]
   (let [log (reverse (:log db-val))
         log (if (:where opts)
               (filter (:where opts) log)
               log)
         log (if (:entities opts)
               (filter (comp (:entities opts) :e) log)
               log)
         log (if (:attributes opts)
               (filter (comp (:attributes opts) :a) log)
               log)]
     (take n log))))

;; ---------------------------------------------------------------------------
;; Projection and aggregation
;; ---------------------------------------------------------------------------

(defn project
  "Derives a small value from a db value, cheap to clone across states.
  spec is either a function (called with db-val) or a map of output key
  to [e a] path, in which case a map of output key to looked-up value
  is returned."
  [db-val spec]
  (if (fn? spec)
    (spec db-val)
    (into {}
          (for [[k path] spec]
            [k (get-in (:entities db-val) path)]))))

(defn merge
  "Merges two db values into a new one. The fact logs concatenate and
  replay in instant order, so the entity view reflects last-write-wins
  by :instant. The tx counter is the max of the two. Schema, closed?,
  indexed-attrs, and history are taken from db-a. Indexes are rebuilt
  for the merged entity view."
  [db-a db-b]
  (let [schema (get db-a :schema {})
        indexed-attrs (get db-a :indexed-attrs #{})
        all-facts (sort-by :instant (concat (:log db-a) (:log db-b)))
        entities (reduce (fn [acc f] (apply-fact acc f schema))
                         {} all-facts)
        indexes (build-indexes entities indexed-attrs)]
    {:entities entities
     :log (vec all-facts)
     :tx (max (:tx db-a) (:tx db-b))
     :schema schema
     :closed? (get db-a :closed? false)
     :indexed-attrs indexed-attrs
     :indexes indexes
     :history (get db-a :history)}))

(defn fold
  "Reduces across a collection of db values. extract-fn pulls the value
  to fold from each db (defaults to entity count); f combines the
  accumulator with each extracted value starting from init."
  ([dbs f init]
   (fold dbs f init (fn [db] (count (:entities db)))))
  ([dbs f init extract-fn]
   (reduce (fn [acc db] (f acc (extract-fn db))) init dbs)))

(defn concat-logs
  "Concatenates the fact logs of the given db values into a single flat
  seq sorted by :instant."
  [& dbs]
  (sort-by :instant (apply concat (map :log dbs))))

;; ---------------------------------------------------------------------------
;; Stats
;; ---------------------------------------------------------------------------

(defn stats
  "Returns a map of store statistics: fact count, entity count, and the
  current tx number."
  [db-val]
  {:facts (count (:log db-val))
   :entities (count (:entities db-val))
   :tx (:tx db-val)})

;; ---------------------------------------------------------------------------
;; Compaction
;; ---------------------------------------------------------------------------

(defn compact
  "Bounds the log of a durable or long-lived store by dropping old
  facts while preserving the materialized view. Does not write to the
  WAL — compaction is a maintenance operation; checkpoint afterwards to
  persist the compacted state.

  listeners do NOT fire for compact (it bypasses the tx log); see
  `listen` for the scope contract.

  With one arg, drops the entire log (only the current view survives).
  With a keep-spec map:
    {:keep-last N}     keep the last N facts
    {:keep-since T}    keep facts at or after instant T"
  ([conn]
   (let [cur @conn]
     (store-commit* conn {:entities (:entities cur)
                          :log []
                          :tx (:tx cur)
                          :schema (get cur :schema {})
                          :closed? (get cur :closed? false)
                          :indexed-attrs (get cur :indexed-attrs #{})
                          :indexes (get cur :indexes {})
                          :history (get cur :history)})))
  ([conn keep-spec]
   (let [valid-last  (and (map? keep-spec)
                          (contains? keep-spec :keep-last)
                          (integer? (:keep-last keep-spec))
                          (>= (:keep-last keep-spec) 0))
         valid-since (and (map? keep-spec)
                          (contains? keep-spec :keep-since)
                          (integer? (:keep-since keep-spec)))]
     (if (or valid-last valid-since)
       nil
       (throw (ex-info (str "compact keep-spec must be {:keep-last N} or {:keep-since T}, got: "
                            (pr-str keep-spec))
                       {::invalid-keep-spec keep-spec}))))
   (let [cur @conn
         log (:log cur)
         kept (cond
                (and (map? keep-spec) (:keep-last keep-spec))
                (take-last (:keep-last keep-spec) log)

                (and (map? keep-spec) (:keep-since keep-spec))
                (filter #(>= (:instant %) (:keep-since keep-spec)) log)

                :else log)]
     (store-commit* conn {:entities (:entities cur)
                          :log (vec kept)
                          :tx (:tx cur)
                          :schema (get cur :schema {})
                          :closed? (get cur :closed? false)
                          :indexed-attrs (get cur :indexed-attrs #{})
                          :indexes (get cur :indexes {})
                          :history (get cur :history)}))))

;; ---------------------------------------------------------------------------
;; Schema and migration
;; ---------------------------------------------------------------------------

(defn schema
  "Returns the store's schema map (attribute -> {:type :cardinality}).
  Empty when no schema was configured."
  [db-val]
  (get db-val :schema {}))

(defn migrate
  "Programmatically changes the store's schema. Validates existing
  facts against the new schema, optionally coerces non-conforming
  values, and publishes the result.

  opts map (all optional):
    :coerce   {attr coerce-fn-sym} — applied before validation
    :force    true — publish even when violations exist
    :indexes  #{attr} — attrs to add to the index set
    :data     fn — called as a tx on the migrated db (for data migration)

  Returns {:db-after new-db :violations [...] :tx N}.
  Throws ::migration-conflict when violations exist without :force.

  listeners do NOT fire for migrate (it publishes via store-commit*,
  bypassing the tx log); see `listen` for the scope contract."
  ([conn new-schema] (migrate conn new-schema {}))
  ([conn new-schema opts]
   (let [cur @conn
         old-schema (get cur :schema {})
         coerce (:coerce opts)
         indexed-attrs (set/union (get cur :indexed-attrs #{})
                                   (or (:indexes opts) #{}))
          entities (if coerce
                     (reduce (fn [ents [e attrs]]
                               (assoc ents e
                                 (reduce (fn [m [a v]]
                                           (if-let [cf (get coerce a)]
                                             (assoc m a ((coerce-fn cf) v))
                                             (assoc m a v)))
                                         {} attrs)))
                             (:entities cur) (:entities cur))
                     (:entities cur))
         violations (vec
                      (for [[e attrs] entities
                            [a v] attrs
                            :let [spec (get new-schema a)]
                            :when (and spec (:type spec)
                                       (not= (:type spec) :any)
                                       (not (type-matches? v (:type spec))))]
                        {:entity e :attr a :value v :expected (:type spec)}))
         _ (when (and (seq violations) (not (:force opts)))
             (throw (ex-info (str "Migration conflict: " (count violations) " violations")
                             {::migration-conflict violations})))
         new-db {:entities entities
                 :log (:log cur)
                 :tx (:tx cur)
                 :schema new-schema
                 :closed? (:closed? cur)
                 :indexed-attrs indexed-attrs
                 :indexes (build-indexes entities indexed-attrs)
                 :entity-specs (:entity-specs cur)
                 :history (:history cur)}
         new-db (if (:data opts)
                  (:db-after (with new-db ((:data opts))))
                  new-db)
         tx-num (:tx cur)
         instant (store-clock* conn)]
     (store-commit* conn new-db {:tx tx-num :instant instant :migration true})
     {:db-after new-db :violations violations :tx tx-num})))

;; ---------------------------------------------------------------------------
;; Datalog query
;; ---------------------------------------------------------------------------

(defn- variable?
  "Returns true if x is a query variable (symbol starting with ?)."
  [x]
  (and (symbol? x)
       (let [s (name x)]
         (and (pos? (count s))
              (= (first s) \?)))))

(defn- parse-query
  "Parses a Datalog query vector into {:find :with :in :order-by :where}.
  Throws ex-info tagged ::invalid-query when the input is not a vector
  beginning with :find."
  [query]
  (when-not (and (vector? query) (= (first query) :find))
    (throw (ex-info "Invalid query: expected a vector starting with :find"
                    {::invalid-query query})))
  (loop [q query find-vars [] with-vars [] in-vars [] order-by nil clauses [] mode nil]
    (if (empty? q)
      {:find find-vars :with with-vars :in in-vars
       :order-by order-by :where clauses}
      (let [el (first q)]
        (cond
          (= el :find)     (recur (rest q) find-vars with-vars in-vars order-by clauses :find)
          (= el :with)     (recur (rest q) find-vars with-vars in-vars order-by clauses :with)
          (= el :in)       (recur (rest q) find-vars with-vars in-vars order-by clauses :in)
          (= el :where)    (recur (rest q) find-vars with-vars in-vars order-by clauses :where)
          (= el :order-by) (recur (rest q) find-vars with-vars in-vars order-by clauses :order-by)
          (= mode :find)     (recur (rest q) (conj find-vars el) with-vars in-vars order-by clauses :find)
          (= mode :with)     (recur (rest q) find-vars (conj with-vars el) in-vars order-by clauses :with)
          (= mode :in)       (recur (rest q) find-vars with-vars (conj in-vars el) order-by clauses :in)
          (= mode :order-by) (if (variable? el)
                               (recur (rest q) find-vars with-vars in-vars {:var el :asc true} clauses :order-by-done)
                               (recur (rest q) find-vars with-vars in-vars (assoc order-by :asc (not= el :desc)) clauses :order-by-done))
          (= mode :order-by-done) (if (= el :desc)
                                    (recur (rest q) find-vars with-vars in-vars (assoc order-by :asc false) clauses :order-by-done)
                                    (recur (rest q) find-vars with-vars in-vars order-by (conj clauses el) :where))
          (= mode :where)    (recur (rest q) find-vars with-vars in-vars order-by (conj clauses el) :where)
          :else              (recur (rest q) find-vars with-vars in-vars order-by clauses mode))))))

(defn- pattern-binding-for
  "Creates a binding map from a pattern match, given the pattern's
  e and v elements and the actual entity-id and value."
  [e v eid val]
  (cond-> {}
    (variable? e) (assoc e eid)
    (variable? v) (assoc v val)))

(defn- pattern-ok?
  "Checks if entity eid with attribute value val is consistent with
  the constant constraints in pattern [e a v]."
  [e v eid val]
  (and (or (variable? e) (= e eid))
       (or (variable? v) (= v val))))

(defn- find-pattern-bindings
  "Finds all variable bindings for a pattern [e a v] in db.
  Constant elements in e or v act as filters. Uses the reverse index
  for attr when available (O(entities-with-attr) instead of O(all
  entities)); falls back to a linear scan otherwise."
  [db [e a v]]
  (if-let [index (get-in db [:indexes a])]
    (if (variable? v)
      (for [[val eids] index
            eid eids
            :when (pattern-ok? e v eid val)]
        (pattern-binding-for e v eid val))
      (let [eids (or (get index v) #{})]
        (for [eid eids
              :when (pattern-ok? e v eid v)]
          (pattern-binding-for e v eid v))))
    (for [[eid attrs] (:entities db)
          :when (contains? attrs a)
          :let [val (get attrs a)]
          :when (pattern-ok? e v eid val)]
      (pattern-binding-for e v eid val))))

(defn- bindings-consistent?
  "Returns true if two bindings agree on all shared keys."
  [b1 b2]
  (every? (fn [[k v]] (or (not (contains? b1 k)) (= (get b1 k) v)))
          b2))

(defn- join-bindings
  "Joins two seqs of bindings, keeping only consistent pairs."
  [old new]
  (mapcat (fn [o]
            (for [n new :when (bindings-consistent? o n)]
              (map-merge o n)))
          old))

(defn- subst-vars
  "Substitutes query variables in an expression with bound values."
  [expr bindings]
  (cond
    (variable? expr) (get bindings expr)
    (vector? expr) (vec (map #(subst-vars % bindings) expr))
    (seq? expr) (apply list (map #(subst-vars % bindings) expr))
    :else expr))

(defn- predicate-clause?
  "Returns true if a where clause is a predicate [(pred ...)]."
  [clause]
  (and (vector? clause)
       (= (count clause) 1)
       (seq? (first clause))
       ;; The predicate head must be a literal symbol resolvable in the
       ;; calling namespace -- never a variable. A variable head would
       ;; let an :in-bound value control the function position of an
       ;; evaluated form, which is a code-injection vector.
       (symbol? (first (first clause)))
       (not (variable? (first (first clause))))))

(defn- not-clause?
  "Returns true if clause is a (not [pattern]) form."
  [clause]
  (and (seq? clause) (= (first clause) 'not)))

(defn- not-join-clause?
  "Returns true if clause is a (not-join [?e ...] [pattern...]) form."
  [clause]
  (and (seq? clause) (= (first clause) 'not-join)))

(defn- or-clause?
  "Returns true if clause is an (or clause...) form."
  [clause]
  (and (seq? clause) (= (first clause) 'or)))

(defn- pattern-vars
  "Returns the set of variable symbols appearing in a pattern clause's
  e and v slots (a is the attribute and assumed constant)."
  [clause]
  (set (for [el [((vec clause) 0) ((vec clause) 2)]
             :when (variable? el)]
         el)))

(defn- validate-query
  "Validates a parsed query. Throws ex-info tagged ::invalid-query for
  malformed clauses, unbound find vars, or an :order-by var that is
  not projected by :find."
  [find where order-by]
  (let [bound-vars (reduce
                     (fn [acc clause]
                       (cond
                         (predicate-clause? clause)
                         (into acc (filter variable? (first clause)))
                         (and (vector? clause) (= (count clause) 3))
                         (into acc (pattern-vars clause))
                         (or-clause? clause)
                         (reduce (fn [a c]
                                   (cond
                                     (and (vector? c) (= (count c) 3))
                                     (into a (pattern-vars c))
                                     (predicate-clause? c)
                                     (into a (filter variable? (first c)))
                                     :else a))
                                 acc (rest clause))
                         (not-clause? clause) acc
                         (not-join-clause? clause) acc
                         :else
                         (throw
                           (ex-info "Invalid Datalog clause: must be [e a v] pattern, [(pred ...)] predicate, (not ...), (not-join ...), or (or ...)"
                                    {::invalid-query clause}))))
                     #{} where)
        find-vars-for-check (flatten
                              (for [v find]
                                (cond
                                  (variable? v) [v]
                                  (and (seq? v) (variable? (second v))) [(second v)]
                                  :else [])))
        unbound (filter #(not (contains? bound-vars %)) find-vars-for-check)]
    (when (seq unbound)
      (throw
        (ex-info (str "Query var(s) not bound by any clause: " (vec unbound))
                 {::invalid-query {:find find :unbound (vec unbound)}})))
    (when (and order-by (variable? (:var order-by)))
      (let [ob-var (:var order-by)]
        (when-not (contains? bound-vars ob-var)
          (throw
            (ex-info (str "Query :order-by var not bound by any clause: " ob-var)
                     {::invalid-query {:order-by ob-var}})))))))

(defn- process-not
  "Filters bindings: removes any binding where the negated pattern matches."
  [db bindings clause]
  (let [pattern (second clause)]
    (filter (fn [b]
              (let [resolved (subst-vars pattern b)
                    matches (find-pattern-bindings db resolved)]
                (empty? matches)))
            bindings)))

(defn- process-not-join
  "Filters bindings using explicit join variables."
  [db bindings clause]
  (let [patterns (drop 2 clause)]
    (filter (fn [b]
              (let [resolved (map #(subst-vars % b) patterns)
                    matches (reduce (fn [ms p]
                                      (join-bindings ms (find-pattern-bindings db p)))
                                    [{}] resolved)]
                (empty? matches)))
            bindings)))

(defn- process-or
  "Unions bindings from alternative branches."
  [db bindings clause]
  (let [alternatives (rest clause)]
    (reduce (fn [acc alt]
              (if (vector? alt)
                (let [matches (find-pattern-bindings db alt)
                      joined (join-bindings bindings matches)]
                  (concat acc joined))
                (let [and-clauses (rest alt)
                      joined (reduce (fn [bs c]
                                       (if (predicate-clause? c)
                                         (filter-by-predicate bs c)
                                         (join-bindings bs (find-pattern-bindings db c))))
                                     bindings
                                     and-clauses)]
                  (concat acc joined))))
            []
            alternatives)))

(defn- filter-by-predicate
  "Filters bindings by applying a predicate clause to each binding.

  The predicate clause has the shape [(pred-sym arg-spec ...)] where
  pred-sym is a literal symbol resolvable in the calling namespace
  and each arg-spec is either a query variable (looked up in the
  binding) or a literal value. The resolved function is APPLY'd to
  the arg values.

  This intentionally replaces an earlier (eval substituted-form)
  implementation. Eval'ing a substituted form let an :in-bound
  value in any arg slot run as code: passing '(println \"pwned\")
  as ?v produced the form (identity (println \"pwned\")) and eval
  ran the inner println. Applying the resolved function to the
  literal arg values closes that vector -- the value is data, never
  an expression to evaluate."
  [bindings clause]
  (let [pred-form (first clause)
        pred-sym  (first pred-form)
        arg-specs (rest pred-form)
        pred-var  (resolve pred-sym)
        _         (when-not pred-var
                    (throw
                      (ex-info (str "Query predicate not resolvable: " pred-sym)
                               {::invalid-query {:predicate pred-sym}})))
        pred-fn   @pred-var
        _         (when-not (fn? pred-fn)
                    (throw
                      (ex-info (str "Query predicate not a function: " pred-sym)
                               {::invalid-query {:predicate pred-sym}})))]
    (filter (fn [b]
              (let [args (for [a arg-specs]
                           (if (variable? a) (get b a) a))]
                (apply pred-fn args)))
            bindings)))

;; ---------------------------------------------------------------------------
;; Aggregates and find specs
;; ---------------------------------------------------------------------------

(def ^:private aggregate-fns
  '#{count count-distinct sum min max avg distinct sample rand})

(defn- aggregate-expr?
  "Returns true if x is an aggregate expression like (count ?e)."
  [x]
  (and (seq? x) (contains? aggregate-fns (first x))))

(defn- detect-find-spec
  "Returns {:spec :rel|:coll|:scalar|:tuple :vars [...]}."
  [find-vars]
  (cond
    (and (= (count find-vars) 1)
         (vector? (first find-vars))
         (= (last (first find-vars)) '...))
    {:spec :coll :vars (vec (butlast (first find-vars)))}

    (and (>= (count find-vars) 2)
         (= (last find-vars) '.))
    {:spec :scalar :vars (vec (butlast find-vars))}

    (and (= (count find-vars) 1)
         (vector? (first find-vars)))
    {:spec :tuple :vars (vec (first find-vars))}

    :else
    {:spec :rel :vars (vec find-vars)}))

(defn- apply-aggregate
  "Applies aggregate fn-sym to a seq of values. min/max/avg return nil
  on an empty seq (Datomic scalar semantics: a scalar aggregate over
  zero bindings is nil); the reducing fn is arity-2, so reducing []
  would otherwise call it with zero arguments, and avg over zero
  bindings is sum/0 (undefined). count/count-distinct/sum are total
  on [] (0/0/0)."
  [fn-sym values]
  (case fn-sym
    count (count values)
    count-distinct (count (distinct values))
    sum (reduce + 0 values)
    min (if (empty? values) nil (reduce (fn [a b] (if (< a b) a b)) values))
    max (if (empty? values) nil (reduce (fn [a b] (if (> a b) a b)) values))
    avg (if (empty? values) nil (/ (reduce + 0 values) (count values)))
    distinct (set values)
    sample (set (take 1 (shuffle values)))
    rand (set (take 1 (shuffle values)))))

(defn- compute-results
  "Given bindings, find spec vars, with-vars, and where clauses, returns
  a seq of result tuples (vectors)."
  [bindings spec-vars with-vars]
  (let [aggs (filter aggregate-expr? spec-vars)
        scalars (remove aggregate-expr? spec-vars)
        group-keys (vec (concat scalars with-vars))]
    (if (empty? aggs)
      (for [b bindings]
        (vec (for [v spec-vars] (get b v))))
      (let [groups (reduce
                     (fn [m b]
                       (let [key (vec (for [gk group-keys] (get b gk)))]
                         (assoc m key (conj (get m key []) b))))
                     {} bindings)
            results
            (for [[key group-bindings] groups]
              (vec
                (concat
                  (for [s scalars] (get (first group-bindings) s))
                  (for [agg aggs]
                    (let [fn-sym (first agg)
                          agg-var (second agg)
                          vals (for [b group-bindings] (get b agg-var))]
                      (apply-aggregate fn-sym vals))))))]
        (if (empty? groups)
          ;; Empty result set: aggregates produce default values
          (if (empty? scalars)
            (let [default-tuple (vec (for [agg aggs]
                                       (apply-aggregate (first agg) [])))]
              [default-tuple])
            [])
          results)))))

(defn- var-index
  "Returns the index of sym in coll, or nil."
  [coll sym]
  (loop [c (seq coll) i 0]
    (cond
      (not (seq c)) nil
      (= (first c) sym) i
      :else (recur (rest c) (inc i)))))

(defn qseq
  "Executes a Datalog query and returns a lazy seq of results.
  Accepts optional :in bindings as extra arguments. See q for docs."
  ([db-val query] (qseq db-val query nil))
  ([db-val query & in-args]
   (let [{:keys [find with in order-by where]} (parse-query query)
         spec-info (detect-find-spec find)
         spec-vars (:vars spec-info)
          _ (validate-query find where order-by)
         in-bindings (if (and in (seq in) (seq in-args))
                       (zipmap in in-args)
                       {})
         result-bindings
         (reduce (fn [bindings clause]
                   (cond
                     (not-clause? clause) (process-not db-val bindings clause)
                     (not-join-clause? clause) (process-not-join db-val bindings clause)
                     (or-clause? clause) (process-or db-val bindings clause)
                     (predicate-clause? clause) (filter-by-predicate bindings clause)
                     :else (let [matches (find-pattern-bindings db-val clause)]
                             (join-bindings bindings matches))))
                 [in-bindings]
                 where)
         sorted-bindings (if order-by
                           (let [sorted (sort-by (fn [b] (get b (:var order-by))) result-bindings)]
                             (if (:asc order-by) sorted (reverse sorted)))
                           result-bindings)
         tuples (compute-results sorted-bindings spec-vars with)]
    (case (:spec spec-info)
      :rel tuples
      :coll (map first tuples)
      :scalar (map first tuples)
      :tuple tuples))))

(defn q
  "Executes a Datalog query against a db value.

  Query format:
    [:find find-spec :in ?var ... :order-by ?var :where clause ...]

  Find specs:
    ?e ?n            relation (set of tuples)
    [?e ...]         collection (seq of single values)
    ?e .             scalar (single value, nil if empty)
    [?e ?n]          single tuple

  Find elements:
    ?var             scalar value
    (agg-fn ?var)    aggregate (count, sum, min, max, avg, distinct, ...)

  Pattern clause: [?e :attr ?v] or [?e :attr literal]
  Predicate clause: [(pred-fn ?x ... literal ...)]
  :with ?var        group-by var omitted from results
  :in ?var ...      bind external values positionally from extra args
  :order-by ?var    sort results by var (append :desc for descending)
  (not [pattern])   negation: filter bindings where pattern matches
  (not-join [?e] [pattern...])  negation with explicit join vars
  (or clause ...)   disjunction: union bindings from alternatives

  Returns a set of tuples (for :rel without :order-by), or a seq
  (for :order-by), or other types for other find specs."
  ([db-val query] (q db-val query nil))
  ([db-val query & in-args]
   (let [parsed (parse-query query)
         spec-info (detect-find-spec (:find parsed))
         results (apply qseq db-val query in-args)]
     (cond
       (and (:order-by parsed) (= (:spec spec-info) :rel)) results
       (= (:spec spec-info) :rel) (set results)
       :else (case (:spec spec-info)
               :coll results
               :scalar (first results)
               :tuple (first results))))))

;; ---------------------------------------------------------------------------
;; Reverse-index reads
;; ---------------------------------------------------------------------------

(defn referring
  "Returns the set of source entity-ids whose value for ref-attr
  equals target-eid."
  [db-val attr target-eid]
  (let [reverse (build-reverse-index (:entities db-val) (:schema db-val))]
    (get-in reverse [attr target-eid] #{})))

(defn referred-by
  "Returns a map of {ref-attr #{source-eid...}} for all ref attrs
  pointing at eid."
  [db-val eid]
  (let [reverse (build-reverse-index (:entities db-val) (:schema db-val))]
    (into {}
      (for [[attr targets] reverse
            :when (contains? targets eid)]
        [attr (get targets eid)]))))

;; ---------------------------------------------------------------------------
;; Pull
;; ---------------------------------------------------------------------------

(defn- find-opt
  "Returns the value following key in a flat seq, or nil."
  [opts k]
  (loop [opts (seq opts)]
    (when (seq opts)
      (if (= (first opts) k)
        (when (seq (rest opts)) (second opts))
        (recur (rest opts))))))

(declare pull-entity)

(defn- pull-with-depth
  "Pulls entity e with all attrs, limiting ref recursion to depth levels."
  [db e depth visited]
  (let [attrs (get-in (:entities db) [e])]
    (cond
      (nil? attrs) nil
      (contains? visited e) {:db/id e}
      (<= depth 1) {:db/id e}
      :else
      (let [visited' (conj visited e)
            depth' (dec depth)]
        (loop [result {:db/id e}
               kvs (seq attrs)]
          (if (empty? kvs)
            result
            (let [[k v] (first kvs)]
              (if (= k :db/id)
                (recur result (rest kvs))
                (if (= :ref (get-in (:schema db) [k :type]))
                  (let [vals (if (set? v) v #{v})
                        pulled (for [target vals]
                                 (pull-with-depth db target depth' visited'))]
                    (recur (assoc result k (if (set? v) (set pulled) (first pulled)))
                           (rest kvs)))
                  (recur (assoc result k v) (rest kvs)))))))))))

(defn- pull-attr-spec
  "Processes one attribute spec from a pull pattern.
  Returns [key val] or [:* attrs-map] for wildcard, or nil."
  [db spec e visited]
  (cond
    (keyword? spec)
    (let [v (get-in (:entities db) [e spec])]
      (when (some? v) [spec v]))

    (= spec '*)
    [:* (dissoc (get-in (:entities db) [e]) :db/id)]

    (vector? spec)
    (let [attr (first spec)
          opts (rest spec)
          rename (or (find-opt opts :as) attr)
          default-val (find-opt opts :default)
          limit-val (find-opt opts :limit)
          offset-val (find-opt opts :offset)
          v (get-in (:entities db) [e attr])]
      (cond
        (and (nil? v) (some? default-val)) [rename default-val]
        (nil? v) nil
        (and (or offset-val limit-val) (set? v))
        (let [s (if offset-val (drop offset-val v) v)
              s (if limit-val (take limit-val s) s)]
          (if (empty? s) nil [rename (set s)]))
        :else [rename v]))

    (map? spec)
    (let [[key-expr value-expr] (first spec)
          real-attr (if (keyword? key-expr) key-expr (first key-expr))
          rename (if (keyword? key-expr)
                   key-expr
                   (or (find-opt (rest key-expr) :as) (first key-expr)))
          v (get-in (:entities db) [e real-attr])]
      (if (nil? v)
        nil
        (let [vals (if (set? v) v #{v})
              is-many (set? v)
              visited' (conj visited e)
              pulled (for [target vals]
                       (cond
                         (contains? visited target) {:db/id target}
                         (number? value-expr)
                         (pull-with-depth db target value-expr visited')
                         (= value-expr '...)
                         (pull-with-depth db target 1000000 visited')
                         (vector? value-expr)
                         (pull-entity db value-expr target visited')
                         :else {:db/id target}))]
          [rename (if is-many (set pulled) (first pulled))])))

    :else nil))

(defn- pull-entity
  [db pattern e visited]
  (let [attrs (get-in (:entities db) [e])]
    (when attrs
      (loop [specs (seq pattern)
             result {:db/id e}]
        (if (empty? specs)
          result
          (let [kv (pull-attr-spec db (first specs) e visited)]
            (cond
              (nil? kv) (recur (rest specs) result)
              (= (first kv) :*) (recur (rest specs) (map-merge result (second kv)))
              :else (recur (rest specs) (assoc result (first kv) (second kv))))))))))

(defn pull
  "Pulls a projection of entity e using a pull pattern.

  Pattern is a vector of attribute specs:
    :attr              return that attr's value
    *                  return all attrs
    [:attr :as name]   rename in output
    [:attr :limit N]   limit cardinality-:many to N values
    [:attr :default v] default for missing attr
    {attr sub-pattern} recurse into ref attr
    {attr N}           recurse with depth limit N
    {attr ...}         recurse with unlimited depth

  Result includes :db/id. Missing attrs are omitted.
  Returns nil for nonexistent entities."
  [db-val pattern e]
  (pull-entity db-val pattern e #{}))

(defn pull-many
  "Pulls the same pattern across a seq of eids, preserving order."
  [db-val pattern eids]
  (for [eid eids]
    (pull-entity db-val pattern eid #{})))

;; ---------------------------------------------------------------------------
;; Sorted range queries
;; ---------------------------------------------------------------------------

;; Classify a value into a comparable category so find-by-range can tell
;; a genuine "out of range" result from a type mismatch. Without this,
;; (>= v lo) on a string value against numeric bounds throws a raw
;; ClassCastException from inside the comparator -- a runtime-bug-shaped
;; error for what is really caller misuse.
(defn- range-value-category
  [x]
  (cond
    (number? x) :number
    (string? x) :string
    (inst? x)   :inst
    (char? x)   :char
    (boolean? x) :boolean
    (keyword? x) :keyword
    (symbol? x)  :symbol
    :else nil))

(defn find-by-range
  "Returns eids whose attr value falls in [lo, hi] (inclusive), in
  ascending value order. Works on any attr; schema :sorted-index true
  documents intent but is not required.

  For :cardinality :many attributes the value is a set; an entity is
  returned if ANY member falls in [lo, hi], ordered by the smallest
  in-range member.

  Bounds and attribute values must be mutually comparable: a numeric
  range over a string attr (or vice versa) throws a classified
  ::range-type-mismatch error rather than a raw ClassCastException."
  [db-val attr lo hi]
  (let [lo-cat (range-value-category lo)
        hi-cat (range-value-category hi)]
    (when (and lo-cat hi-cat (not= lo-cat hi-cat))
      (throw
        (ex-info (str "find-by-range: lo and hi must be the same type, got "
                      (pr-str lo) " and " (pr-str hi))
                 {:reason :range-type-mismatch :attr attr :lo lo :hi hi})))
    (->> (:entities db-val)
         (keep (fn [[e attrs]]
                 (let [v (get attrs attr)]
                   (cond
                     (nil? v) nil
                     ;; A value (or set member) whose category differs
                     ;; from the bounds is a misuse: surface it rather
                     ;; than letting (>= v lo) throw opaquely.
                     (set? v) (let [bad (some #(when (and lo-cat
                                                           (not= (range-value-category %) lo-cat))
                                                 %) v)]
                                (when bad
                                  (throw
                                    (ex-info (str "find-by-range: value " (pr-str bad)
                                                  " for " attr " is not comparable to bounds "
                                                  (pr-str lo) ".." (pr-str hi))
                                             {:reason :range-type-mismatch
                                              :attr attr :value bad :entity e})))
                                (let [in-range (filter #(and (>= % lo) (<= % hi)) v)]
                                  (when (seq in-range)
                                    [e (reduce min in-range)])))
                     (and lo-cat (not= (range-value-category v) lo-cat))
                     (throw
                       (ex-info (str "find-by-range: value " (pr-str v) " for " attr
                                     " is not comparable to bounds " (pr-str lo) ".." (pr-str hi))
                                {:reason :range-type-mismatch
                                 :attr attr :value v :entity e}))
                     (and (>= v lo) (<= v hi)) [e v]
                     :else nil))))
         (sort-by second)
         (map first))))

;; ---------------------------------------------------------------------------
;; Raw datom access
;; ---------------------------------------------------------------------------

(defn datoms
  "Returns a seq of {:e :a :v} maps in the named index order.
  Index: :eavt (entity, attr), :avet (attr, value), :aevt (attr, entity)."
  [db-val index]
  (let [entries (for [[e attrs] (:entities db-val)
                      [a v] attrs
                      :when (not= a :db/id)]
                  {:e e :a a :v v})]
    (case index
      :eavt (sort-by (fn [d] [(:e d) (:a d)]) entries)
      :avet (sort-by (fn [d] [(:a d) (:v d)]) entries)
      :aevt (sort-by (fn [d] [(:a d) (:e d)]) entries)
      (throw (ex-info (str "Unknown index: " index)
                      {:index index})))))
