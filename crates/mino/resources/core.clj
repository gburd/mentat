;; mino core library
;; Compiled into the binary at build time via core_mino.h.

;; --- Macros ---

(defmacro when
  "Evaluates body when test is truthy. Returns nil otherwise."
  [test & body]
  `(if ~test (do ~@body)))

(defmacro cond
  "Takes pairs of test/expr. Returns the expr for the first truthy test."
  [& clauses]
  (if (< (count clauses) 2)
    nil
    `(if ~(first clauses)
         ~(first (rest clauses))
         (cond ~@(rest (rest clauses))))))

(defmacro and
  "Returns the first falsy value, or the last value if all are truthy."
  [& xs]
  (if (= 0 (count xs))
    true
    (if (= 1 (count xs))
      (first xs)
      (let [g (gensym)]
        `(let [~g ~(first xs)]
           (if ~g (and ~@(rest xs)) ~g))))))

(defmacro or
  "Returns the first truthy value, or the last value if none are truthy."
  [& xs]
  (if (= 0 (count xs))
    nil
    (if (= 1 (count xs))
      (first xs)
      (let [g (gensym)]
        `(let [~g ~(first xs)]
           (if ~g ~g (or ~@(rest xs))))))))

(defmacro ->
  "Thread-first. Inserts x as the second item in the first form, then
  inserts the result as the second item in the next form, and so on."
  [x & forms]
  (if (= 0 (count forms))
    x
    (let [step (first forms)]
      (if (cons? step)
        `(-> (~(first step) ~x ~@(rest step)) ~@(rest forms))
        `(-> (~step ~x) ~@(rest forms))))))

(defmacro ->>
  "Thread-last. Inserts x as the last item in the first form, then
  inserts the result as the last item in the next form, and so on."
  [x & forms]
  (if (= 0 (count forms))
    x
    (let [step (first forms)]
      (if (cons? step)
        `(->> (~(first step) ~@(rest step) ~x) ~@(rest forms))
        `(->> (~step ~x) ~@(rest forms))))))

(defmacro dosync
  "Runs body in an STM transaction. Refs may be altered, set, ensured,
  or commuted within. The transaction retries on write conflicts in
  multi-threaded mode. Side effects via (io! ...) inside the body
  throw. Requires STM to be installed (mino_install_stm)."
  [& exprs]
  `(dosync* (fn [] ~@exprs)))

(defmacro sync
  "Like dosync: runs the exprs (which may be nil) in an STM
  transaction. flags-ignored is accepted for arglist parity and
  currently ignored."
  [flags-ignored-for-now & body]
  `(dosync ~@body))

(defmacro io!
  "If invoked within an STM transaction, throws an
  IllegalStateException-equivalent before evaluating body. Marks a
  body as having unsafe side effects so dosync can refuse it."
  [& body]
  `(do (io!-check) ~@body))

;; --- Definitions ---

;; defn is lifted above the early type predicates so they can use it.
;; Anything that depends only on special forms, primitives, and the
;; macros defined above (when, cond, and, or, ->, ->>) is fair game.

;; The :pre/:post condition rewrite lives in the fn special form (C
;; core, eval/fn.c fn_rewrite_prepost_body), so defn, plain fn, and
;; named fn all enforce conditions through one shared mechanism. defn
;; therefore hands its raw arities straight to fn.

(defmacro defn
  "Defines a named function. Supports docstrings, multi-arity, and
   :pre/:post conditions."
  [name & fdecl]
  (let [has-doc  (string? (first fdecl))
        doc      (if has-doc (first fdecl) nil)
        fdecl    (if has-doc (rest fdecl) fdecl)
        has-attr (map? (first fdecl))
        fdecl    (if has-attr (rest fdecl) fdecl)
        ;; If the first remaining form is a vector, this is single-arity
        ;; (params-vec body...). Otherwise it's a sequence of arity
        ;; lists (params-vec body...) (params-vec body...). fn applies
        ;; the :pre/:post rewrite for both shapes.
        form     (if (vector? (first fdecl))
                   (cons 'fn fdecl)
                   (apply list 'fn fdecl))
        ;; The declared parameter vectors in declaration order, stored
        ;; under :arglists on the var the way JVM defn does. The value
        ;; is quoted so def's metadata evaluation yields the raw forms.
        arglists (if (vector? (first fdecl))
                   (list (first fdecl))
                   (apply list (mapv first fdecl)))
        name     (vary-meta name assoc :arglists (list 'quote arglists))]
    (if doc
      `(def ~name ~doc ~form)
      `(def ~name ~form))))

(defmacro defn-
  "Same as defn, yielding a non-public def."
  [name & decls]
  (apply list 'defn (vary-meta name assoc :private true) decls))

(defmacro defonce
  "Defines name only if it has no root binding."
  [name expr]
  `(when-not (resolve '~name)
     (def ~name ~expr)))

;; --- Logic and utilities ---

;; not is a C primitive.
(defn not=
  "Returns true if the arguments are not equal."
  ([x] false)
  ([x y] (not (= x y)))
  ([x y & more] (not (apply = x y more))))

(defn identity "Returns its argument." [x] x)

;; list is a C primitive.
;; empty? is a C primitive.
;; >, <=, >= are C primitives (compare_chain) - see prim/numeric.c

;; --- Type predicates ---
;; Most primitive type predicates (nil?, cons?, string?, number?, keyword?,
;; symbol?, vector?, map?, fn?, set?, seq?) are defined as C primitives
;; for speed. ifn? is kept here since it combines multiple predicates.

(defn ifn?
  "Returns true if x can be called as a function."
  [x]
  (or (fn? x) (keyword? x) (map? x) (vector? x)
      (set? x) (symbol? x) (var? x) (future? x)))

;; --- Qualified symbol/keyword predicates ---

(defn qualified-symbol?
  "Returns true if x is a namespace-qualified symbol."
  [x]
  (and (symbol? x) (some? (namespace x))))

(defn simple-symbol?
  "Returns true if x is a symbol with no namespace."
  [x]
  (and (symbol? x) (nil? (namespace x))))

(defn qualified-keyword?
  "Returns true if x is a namespace-qualified keyword."
  [x]
  (and (keyword? x) (some? (namespace x))))

(defn simple-keyword?
  "Returns true if x is a keyword with no namespace."
  [x]
  (and (keyword? x) (nil? (namespace x))))

;; --- Atoms ---

;; swap! is defined as a C primitive; no mino-level fallback needed.

;; vswap! is a macro, not a function: it expands to a vreset! of f applied
;; to the dereferenced volatile, so the extra args never box into a
;; variadic apply. vol is referenced twice (deref and vreset!), matching
;; JVM Clojure, which likewise evaluates the volatile expression twice.
(defmacro vswap!
  "Non-atomically swaps the value of the volatile to be:
   (apply f current-value-of-vol args). Returns the new value."
  [vol f & args]
  `(vreset! ~vol (~f (deref ~vol) ~@args)))

;; --- Lazy sequence operations ---

(def ^:private map1 lazy-map-1)

(defn- all-some? [coll]
  (if (empty? coll)
    true
    (if (first coll)
      (all-some? (rest coll))
      false)))

(defn- map-n [f seqs]
  (lazy-seq
    (let [ss (map1 seq seqs)]
      (when (all-some? ss)
        (cons (apply f (map1 first ss))
              (map-n f (map1 rest ss)))))))

(defn map
  "Returns a lazy sequence of applying f to each item in coll. When
   called with multiple collections, maps f across them in parallel.
   When called with no collection, returns a transducer."
  ([f]
   (with-meta
     (fn [rf]
       (fn ([] (rf))
           ([result] (rf result))
           ([result input] (rf result (f input)))
           ([result input & inputs] (rf result (apply f input inputs)))))
     {:mino.fusion/stage {:kind :map :f f}}))
  ([f & colls]
   (if (= (count colls) 1)
     (map1 f (first colls))
     (map-n f colls))))

(defn filter
  "Returns a lazy sequence of items in coll for which pred returns
   truthy. When called with no collection, returns a transducer."
  ([pred]
   (with-meta
     (fn [rf]
       (fn ([] (rf))
           ([result] (rf result))
           ([result input]
            (if (pred input)
              (rf result input)
              result))))
     {:mino.fusion/stage {:kind :filter :f pred}}))
  ([pred coll] (lazy-filter pred coll)))

(defn take
  "Returns a lazy sequence of the first n items in coll. When called
   with no collection, returns a transducer."
  ([n]
   (fn [rf]
     (let [remaining (volatile! n)]
       (fn ([] (rf))
           ([result] (rf result))
           ([result input]
            (let [r @remaining]
              (vswap! remaining dec)
              (if (> r 0)
                (let [ret (rf result input)]
                  (if (= (dec r) 0)
                    ;; ensure-reduced: a downstream rf may already have
                    ;; wrapped; wrapping again would smuggle a Reduced
                    ;; past the loop's single unwrap.
                    (if (reduced? ret) ret (reduced ret))
                    ret))
                result)))))))
  ([n coll] (lazy-take n coll)))

(defn drop
  "Returns a lazy sequence of all but the first n items in coll. When
   called with no collection, returns a transducer."
  ([n]
   (fn [rf]
     (let [remaining (volatile! n)]
       (fn ([] (rf))
           ([result] (rf result))
           ([result input]
            (if (> @remaining 0)
              (do (vswap! remaining dec) result)
              (rf result input)))))))
  ([n coll] (drop-seq n coll)))

(defn concat
  "Returns a lazy sequence of the concatenation of the given
   collections."
  [& colls]
  (lazy-seq
    (when (seq colls)
      (let [s (seq (first colls))]
        (if s
          (cons (first s) (apply concat (cons (rest s) (rest colls))))
          (apply concat (rest colls)))))))

;; range is a C primitive.

(defn repeat
  "Returns a lazy sequence of xs. With two args, returns n repetitions
   of x. n must be a number; floats and ratios truncate toward zero
   (so 3.14 and 3.99 both yield three repetitions), matching JVM
   Clojure's RT/longCast. Booleans, strings, keywords, and other
   non-numeric counts throw."
  ([x]
   (lazy-seq (cons x (repeat x))))
  ([n x]
   (let [n (cond
             (integer? n)         n
             (number? n)          (long n)
             :else (throw (str "repeat: count must be a number, got "
                               (type n))))]
     (lazy-seq
       (when (> n 0)
         (cons x (repeat (- n 1) x)))))))

;; --- Collection utilities ---

(defn update
  "Updates the value at key k in map m by applying f to the old value
   and any args."
  [m k f & args]
  (assoc m k (apply f (get m k) args)))

;; --- Utility functions ---
;; some, every?, not-any?, not-every? are registered as C primitives
;; (see src/prim/core/sequences.c). zipmap is also a C prim.

;; --- Higher-order functions ---

;; comp, partial, complement are registered as C primitives
;; (see src/prim/core/sequences.c).

;; --- Trivial compositions ---

(defn second "Returns the second item in coll." [x] (first (rest x)))
(defn ffirst
  "Returns the first item of the first item in coll."
  [x] (first (first x)))
;; inc and dec are C primitives.
;; zero? is a C primitive.
(defn ==
  "Returns true if nums are numerically equal, treating ints and floats
   uniformly."
  ([x] true)
  ([x y] (= (+ 0.0 x) (+ 0.0 y)))
  ([x y & more]
   (if (== x y)
     (apply == y more)
     false)))
;; pos?, neg?, even?, odd? are C primitives.
(defn abs
  "Returns the absolute value of x. Matches JVM Math/abs 2's-complement
   semantics: (abs Long/MIN_VALUE) returns Long/MIN_VALUE rather than
   overflowing, since the true absolute value is unrepresentable in a
   signed 64-bit int."
  [a]
  (cond
    (neg? a) (if (int? a) (unchecked-negate a) (- a))
    ;; -0.0 is not neg?; adding 0.0 normalizes it to positive zero
    ;; and is the identity for every other non-negative float.
    (and (float? a) (zero? a)) (+ a 0.0)
    :else a))
(defn max "Returns the greatest of the given values."
                  ([x] x)
                  ([x y] (if (NaN? x) x (if (NaN? y) y (if (> x y) x y))))
                  ([x y & more] (reduce max (max x y) more)))
(defn min "Returns the least of the given values."
                  ([x] x)
                  ([x y] (if (NaN? x) x (if (NaN? y) y (if (< x y) x y))))
                  ([x y & more] (reduce min (min x y) more)))
(defn min-key "Returns the x for which (k x) is least."
                  ([k x] x)
                  ([k x y] (if (< (k x) (k y)) x y))
                  ([k x y & more]
                   ;; Mirrors clojure.core/min-key: the variadic case
                   ;; uses (<= kw kv) so NaN comparisons (always false)
                   ;; keep the running minimum rather than swapping it
                   ;; for the NaN. Different from the 2-arg case, but
                   ;; that matches Clojure's spec semantics.
                   (let [kx (k x) ky (k y)
                         [v kv] (if (< kx ky) [x kx] [y ky])]
                     (loop [v v kv kv more more]
                       (if (seq more)
                         (let [w (first more) kw (k w)]
                           (if (<= kw kv)
                             (recur w kw (next more))
                             (recur v kv (next more))))
                         v)))))
(defn max-key "Returns the x for which (k x) is greatest."
                  ([k x] x)
                  ([k x y] (if (> (k x) (k y)) x y))
                  ([k x y & more]
                   (let [kx (k x) ky (k y)
                         [v kv] (if (> kx ky) [x kx] [y ky])]
                     (loop [v v kv kv more more]
                       (if (seq more)
                         (let [w (first more) kw (k w)]
                           (if (>= kw kv)
                             (recur w kw (next more))
                             (recur v kv (next more))))
                         v)))))
(defn not-empty
  "Returns coll if it has items, nil otherwise."
  [coll] (if (seq coll) coll nil))
(defn constantly "Returns a function that always returns x." [x] (fn [& _] x))
(defn boolean "Coerces x to a boolean value." [x] (if x true false))
;; seq? is defined as a C primitive; no mino-level fallback needed.

;; --- Collection utilities ---

(defn merge
  "Returns a map that is the merge of the maps. If a key occurs in more
  than one map, the mapping from the latter (left-to-right) will be
  the mapping in the result. Per Clojure, position-2+ args use `conj`
  semantics, so MapEntries / 2-element vectors are accepted."
  [& maps]
  (when (some identity maps)
    (reduce (fn [acc m] (conj (or acc {}) m)) maps)))

(defn select-keys
  "Returns a map containing only the entries whose keys are in ks."
  [map keyseq]
  ;; Validate ks is a collection. A bare scalar like a single keyword
  ;; would otherwise silently produce {} because (reduce ... :a) sees
  ;; nothing.
  (let [_ (seq keyseq)]
    (reduce (fn [acc k]
      (if (contains? map k)
        (assoc acc k (get map k))
        acc))
      (with-meta {} (meta map)) keyseq)))

;; zipmap is registered as a C primitive (see src/prim/core/sequences.c).

;; frequencies is registered as a C primitive (see src/prim/core/sequences.c).

;; group-by is registered as a C primitive (see src/prim/core/sequences.c).

;; --- More higher-order ---

;; juxt is registered as a C primitive (see src/prim/core/sequences.c).

(defn mapcat
  "Returns the result of applying concat to the result of mapping f
   over coll. When called with no collection, returns a transducer."
  ([f] (comp (map f) cat))
  ([f coll]
   ;; Validate eagerly: f must be invokable, coll must be seqable.
   (when-not (ifn? f)
     (throw (str "mapcat: f must be a function, got " (type f))))
   (let [_ (seq coll)
         cat-lazy (fn cat-lazy [s]
                    (lazy-seq
                      (when (seq s)
                        (let [inner (seq (f (first s)))]
                          (if inner
                            (concat inner (cat-lazy (rest s)))
                            (cat-lazy (rest s)))))))]
     (cat-lazy coll)))
  ([f c1 c2]
   (mapcat (fn [pair] (apply f pair))
           (map vector c1 c2)))
  ([f c1 c2 & colls]
   (mapcat (fn [args] (apply f args))
           (apply map vector c1 c2 colls))))

;; --- Lazy combinators ---

(defn take-while
  "Returns a lazy sequence of items from coll while pred returns
   truthy. When called with no collection, returns a transducer."
  ([pred]
   (fn [rf]
     (fn ([] (rf))
         ([result] (rf result))
         ([result input]
          (if (pred input)
            (rf result input)
            (reduced result))))))
  ([pred coll]
   (lazy-seq
     (let [s (seq coll)]
       (when (and s (pred (first s)))
         (cons (first s) (take-while pred (rest s))))))))

(defn drop-while
  "Returns a lazy sequence of items from coll after pred returns
   falsy. When called with no collection, returns a transducer."
  ([pred]
   (fn [rf]
     (let [dropping (volatile! true)]
       (fn ([] (rf))
           ([result] (rf result))
           ([result input]
            (if (and @dropping (pred input))
              result
              (do (vreset! dropping false)
                  (rf result input))))))))
  ([pred coll]
   (let [step (fn [pred coll]
                (let [s (seq coll)]
                  (if (and s (pred (first s)))
                    (recur pred (rest s))
                    s)))]
     (lazy-seq (step pred coll)))))

(defn take-nth
  "Returns a lazy sequence of every nth item in coll. When called with
   no collection, returns a transducer."
  ([n]
   (fn [rf]
     (let [i (volatile! -1)]
       (fn ([] (rf))
           ([result] (rf result))
           ([result input]
            (vswap! i inc)
            (if (= 0 (mod @i n))
              (rf result input)
              result))))))
  ([n coll]
   (lazy-seq
     (let [s (seq coll)]
       (when s
         (cons (first s) (take-nth n (drop n s))))))))

(defmacro lazy-cat
  "Expands to code that yields a lazy concatenation of the given
   collections."
  [& colls]
  (if (seq colls)
    `(lazy-seq (concat ~(first colls) (lazy-cat ~@(rest colls))))
    `(lazy-seq nil)))

(defn iterate
  "Returns a lazy sequence of x, (f x), (f (f x)), and so on. f is
   applied only when the tail is realized: element 0 is x itself, so
   taking n elements calls f exactly (dec n) times."
  [f x]
  (cons x (lazy-seq (iterate f (f x)))))

(defn iteration
  "Creates a seqable via repeated calls to step, a function of some
   continuation token 'k'. The first call to step is passed initk,
   returning 'ret'. If (somef ret) is true, (vf ret) is included in
   the iteration; else iteration terminates and vf/kf are not called.
   If (kf ret) is non-nil it is passed to the next step call; else
   iteration terminates. Used to consume APIs that return paginated
   or batched data.

   step  - (possibly impure) fn of 'k' -> 'ret'
   :somef - fn of 'ret' -> truthy/falsy, default some?
   :vf    - fn of 'ret' -> 'v', default identity
   :kf    - fn of 'ret' -> 'next-k' or nil, default identity
   :initk - first value passed to step, default nil

   Step with non-initk is presumed unreproducible. The first step
   call is deferred until the result is realized."
  [step & {:keys [somef vf kf initk]
           :or   {vf identity, kf identity, somef some?}}]
  (lazy-seq
    ((fn next-iter [ret]
       (when (somef ret)
         (cons (vf ret)
               (when-some [k (kf ret)]
                 (lazy-seq (next-iter (step k)))))))
     (step initk))))

(defn cycle
  "Returns a lazy infinite sequence of repetitions of the items in
   coll."
  [coll]
  ;; Validate eagerly so non-seqable inputs surface a type error at the
  ;; call site rather than only when the result is realised.
  (let [_ (seq coll)]
    (letfn [(cycle-impl [orig coll]
              (lazy-seq
                (let [s (seq coll)]
                  (if s
                    (cons (first s) (cycle-impl orig (rest s)))
                    (when (seq orig)
                      (cycle-impl orig orig))))))]
      (cycle-impl coll coll))))

(defn repeatedly
  "Returns a lazy sequence of calls to f. With two args, returns n
   calls."
  ([f]   (lazy-seq (cons (f) (repeatedly f))))
  ([n f] (take n (repeatedly f))))

(def ^:private interleave2
  (fn interleave2 [c1 c2]
    (lazy-seq
      (let [s1 (seq c1) s2 (seq c2)]
        (when (and s1 s2)
          (cons (first s1)
                (cons (first s2)
                      (interleave2 (rest s1)
                                   (rest s2)))))))))

(defn interleave
  "Returns a lazy sequence of the first item in each collection, then
   the second, and so on."
  ([] ())
  ([c1] (lazy-seq (seq c1)))
  ([c1 c2] (interleave2 c1 c2))
  ([c1 c2 & colls]
   (lazy-seq
     (let [ss (map seq (cons c1 (cons c2 colls)))]
       (when (every? identity ss)
         (concat (map first ss)
                 (apply interleave (map rest ss))))))))

(defn interpose
  "Returns a lazy sequence of the items in coll separated by sep. When
   called with no collection, returns a transducer."
  ([sep]
   (fn [rf]
     (let [started (volatile! false)]
       (fn ([] (rf))
           ([result] (rf result))
           ([result input]
            (if @started
              (let [r (rf result sep)]
                (if (reduced? r) r (rf r input)))
              (do (vreset! started true)
                  (rf result input))))))))
  ([sep coll]
   (lazy-seq
     (let [s (seq coll)]
       (when s
         (cons (first s)
           (mapcat (fn [x] (list sep x)) (rest s))))))))

(defn distinct
  "Returns a lazy sequence of the distinct items in coll. When called
   with no collection, returns a transducer."
  ([]
   (fn [rf]
     (let [seen (volatile! #{})]
       (fn ([] (rf))
           ([result] (rf result))
           ([result input]
            (if (contains? @seen input)
              result
              (do (vswap! seen conj input)
                  (rf result input))))))))
  ([coll]
   (let [step (fn step [xs seen]
                (lazy-seq
                  ((fn [xs seen]
                     (when-let [s (seq xs)]
                       (let [f (first s)]
                         (if (contains? seen f)
                           (recur (rest s) seen)
                           (cons f (step (rest s) (conj seen f)))))))
                   xs seen)))]
     (step coll #{}))))

(def ^:private part-impl
  (fn part-impl [n step coll]
    (lazy-seq
      (when-let [s (seq coll)]
        (let [p (doall (take n s))]
          (when (= n (count p))
            (cons p (part-impl n step (drop step s)))))))))

(def ^:private part-pad-impl
  (fn part-pad-impl [n step pad coll]
    (lazy-seq
      (when-let [s (seq coll)]
        (let [p (doall (take n s))]
          (if (= n (count p))
            (cons p (part-pad-impl n step pad (drop step s)))
            (list (take n (concat p pad)))))))))

(defn partition
  "Returns a lazy sequence of lists of n items each, at offsets step
   apart. With pad, the final partition is filled from pad to reach
   n; if pad is shorter than needed, returns a partition with fewer
   than n items."
  ([n coll]          (part-impl n n coll))
  ([n step coll]     (part-impl n step coll))
  ([n step pad coll] (part-pad-impl n step pad coll)))

(defn partition-by
  "Splits coll into lazy sequences of consecutive items with the same
   (f item) value. When called with no collection, returns a
   transducer."
  ([f]
   (fn [rf]
     (let [buf (volatile! [])
           pval (volatile! ::none)]
       (fn ([] (rf))
           ([result]
            (let [b @buf]
              (if (empty? b)
                (rf result)
                (let [r (rf result b)]
                  (rf (unreduced r))))))
           ([result input]
            (let [v (f input)
                  p @pval]
              (vreset! pval v)
              (if (or (= p ::none) (= v p))
                (do (vswap! buf conj input) result)
                (let [b @buf]
                  (vreset! buf [input])
                  (rf result b)))))))))
  ([f coll]
   (lazy-seq
     (let [s (seq coll)]
       (when s
         (let [v (f (first s))
               run (cons (first s)
                     (take-while (fn [x] (= (f x) v)) (rest s)))
               remaining (drop (count run) s)]
           (cons run (partition-by f remaining))))))))

;; --- Forcing ---

;; doall and dorun are C primitives.

;; --- Type predicates and trivial compositions ---
;; true?, false?, boolean?, int?, float?, char? are C primitives.

(defn integer?
  "Returns true if x is an integer (long or bigint)."
  [n] (if (mino-installed? :bignum)
        (or (int? n) (bigint? n))
        (int? n)))
;; pos-int? / neg-int? / nat-int? specifically test the long-sized
;; int tier per Clojure's contract: `(neg-int? -1N)` returns false on
;; the JVM because clojure.core/neg-int? composes int? (Long-only),
;; not the broader integer?.
(defn pos-int?
  "Returns true if x is a positive integer (long tier)."
  [x] (and (int? x) (pos? x)))
(defn neg-int?
  "Returns true if x is a negative integer (long tier)."
  [x] (and (int? x) (neg? x)))
(defn nat-int?
  "Returns true if x is a non-negative integer (long tier)."
  [x] (and (int? x) (not (neg? x))))
(defn double?
  "Returns true if x is a 64-bit double (mino's `:float` tier).
   Distinct from `float?`, which also returns true for the 32-bit
   `:float32` tier produced by `(float x)`. Matches JVM Clojure
   where `double?` is `(instance? Double x)`."
  [x] (= :float (type x)))
;; ratio? / rational? / decimal? are C primitives that consult the real
;; numeric-tower types (MINO_RATIO, MINO_BIGDEC); registered in prim.c.
;; double is a C primitive that returns a 64-bit MINO_FLOAT;
;; registered in prim/numeric.c alongside int / long / short / byte.
(defn num
  "Returns x if it is a number, nil if x is nil, otherwise throws.
   Nil pass-through matches the cross-dialect convention used by the
   `:default` arm of `clojure.core-test.num` (no-op on numeric inputs
   plus nil), where JVM Clojure's NPE-on-nil is platform-specific."
  [x] (cond (nil? x)    nil
            (number? x) x
            :else       (throw "num expects a number")))
(defn coll?
  "Returns true if x is a collection."
  [x] (or (seq? x) (vector? x) (map? x) (set? x) (= :queue (type x))))
;; some? is a C primitive.
;; list? is registered as a C primitive (see src/prim/core/reflection.c)
;; that distinguishes MINO_CONS / MINO_EMPTY_LIST from
;; MINO_CHUNKED_CONS, matching Clojure's narrower contract: sequences
;; produced by `seq` on other collections are seqs but not lists.
;; atom? is defined as a C primitive; no mino-level fallback needed.
;; not-any? / not-every? are C primitives (see src/prim/core/sequences.c).
;; distinct? is registered as a C primitive (see src/prim/core/sequences.c).
;; Alias of the hash-map prim: :arglists ride def metadata because the
;; value is a var, not a fn form defn could derive them from.
(def ^{:arglists '([] [& keyvals])} array-map
  "Creates a hash-map." hash-map)
(defn sorted?
  "Returns true if x is a sorted collection."
  [coll]
  (let [t (type coll)] (or (= t :sorted-map) (= t :sorted-set))))
(defn associative?
  "Returns true if x supports assoc (maps and vectors)."
  [coll]
  (let [t (type coll)]
    (or (= t :map) (= t :vector) (= t :sorted-map) (= t :map-entry))))
(defn reversible?
  "Returns true if x supports rseq (vectors and sorted collections)."
  [coll] (let [t (type coll)]
        (or (= t :vector) (= t :sorted-map) (= t :sorted-set)
            (= t :map-entry))))
(defn any? "Returns true for any argument." [x] true)
(defn seqable?
  "Returns true if (seq x) is supported."
  [x] (or (nil? x) (coll? x) (string? x)
          (instance? :object-array x)
          (instance? :int-array x)
          (instance? :long-array x)
          (instance? :short-array x)
          (instance? :byte-array x)
          (instance? :float-array x)
          (instance? :double-array x)
          (instance? :char-array x)
          (instance? :boolean-array x)))
(defn indexed?
  "Returns true if x supports nth in constant time (vectors)."
  [coll] (vector? coll))

;; --- Delay (lazy thunk) ---

(defn delay?
  "Returns true if x is a delay."
  [x] (= :delay (type x)))
(defmacro delay
  "Creates a delay that evaluates body on first deref. The body runs
  at most once: a failure is recorded and rethrown on every later
  force."
  [& body]
  `(delay* (fn [] ~@body)))
(defn force
  "Forces evaluation of a delay. If x is not a delay, returns x."
  [x] (if (delay? x) (deref x) x))

;; --- Monitors (locking) ---

(def monitor-registry
  ;; Vector of [monitor-object owner-thread-id depth] entries. Identity
  ;; keyed (identical?) like canonical monitors; held-monitor counts
  ;; are tiny, so a linear scan is the simple correct structure. The
  ;; single-mutator scheduling model makes each swap! a critical
  ;; section on its own: claims and releases cannot interleave with
  ;; another thread's between yield points.
  (atom []))

(defn monitor-try-enter
  "Claim x for owner, or reenter if owner already holds it. Returns
  true when the claim succeeded, false when another thread holds x."
  [x owner]
  (let [claimed (volatile! false)]
    (swap! monitor-registry
           (fn [entries]
             (let [idx (loop [i 0]
                         (cond
                           (>= i (count entries)) nil
                           (identical? (nth (nth entries i) 0) x) i
                           :else (recur (inc i))))]
               (cond
                 (nil? idx)
                 (do (vreset! claimed true)
                     (conj entries [x owner 1]))

                 (= (nth (nth entries idx) 1) owner)
                 (do (vreset! claimed true)
                     (update entries idx (fn [[o w d]] [o w (inc d)])))

                 :else
                 (do (vreset! claimed false)
                     entries)))))
    @claimed))

(defn monitor-exit
  "Release one level of x for owner; drops the entry at depth zero."
  [x owner]
  (swap! monitor-registry
         (fn [entries]
           (let [idx (loop [i 0]
                       (cond
                         (>= i (count entries)) nil
                         (and (identical? (nth (nth entries i) 0) x)
                              (= (nth (nth entries i) 1) owner)) i
                         :else (recur (inc i))))]
             (cond
               (nil? idx) entries
               (> (nth (nth entries idx) 2) 1)
               (update entries idx (fn [[o w d]] [o w (dec d)]))
               :else
               (vec (concat (subvec entries 0 idx)
                            (subvec entries (inc idx))))))))
  nil)

(defmacro locking
  "Executes body while holding a monitor of x. Reentrant per thread;
  released on normal exit and on throw. Exclusion is cooperative:
  contending threads wait for the holder to release across yield
  points."
  [x & body]
  `(let [mon# ~x
         owner# (mino-thread-id*)]
     (loop []
       (when-not (monitor-try-enter mon# owner#)
         (thread-sleep 1)
         (recur)))
     (try
       (do ~@body)
       (finally
         (monitor-exit mon# owner#)))))
;; --- Sequence navigation ---

(defn next
  "Returns a seq of the items after the first. Returns nil if no more
   items."
  [coll] (seq (rest coll)))
(defn nfirst "Same as (next (first coll))." [x] (next (first x)))
(defn fnext "Same as (first (next coll))." [x] (first (next x)))
(defn nnext "Same as (next (next coll))." [x] (next (next x)))

;; --- Map entry accessors ---

(defn key
  "Returns the key of a map entry. Throws on values that are not
   map entries (a literal 2-vector, for instance)."
  [e]
  (if (= :map-entry (type e))
    (first e)
    (throw (str "key: expected a map entry, got " (type e)))))
(defn val
  "Returns the value of a map entry. Throws on values that are not
   map entries (a literal 2-vector, for instance)."
  [e]
  (if (= :map-entry (type e))
    (second e)
    (throw (str "val: expected a map entry, got " (type e)))))

(defn counted?
  "Returns true if (count x) is a constant-time operation: vectors,
   maps, sets, and their sorted variants. The empty list also counts
   in constant time. Cons lists and lazy seqs report false because
   their count walks the sequence; strings report false per canon
   (their count walks the character data)."
  [coll]
  (let [t (type coll)]
    (or (= t :vector) (= t :map) (= t :set)
        (= t :sorted-map) (= t :sorted-set) (= t :map-entry)
        (= t :queue)
        (and (= t :list) (empty? coll)))))

(defn bounded-count
  "Returns the count of coll, but stops counting at n."
  [n coll]
  (if (counted? coll)
    (count coll)
    (loop [i 0 s (seq coll)]
      (if (and s (< i n))
        (recur (inc i) (next s))
        i))))

;; --- Collection conversions ---

(defn remove
  "Returns a lazy sequence of items in coll for which pred returns
   falsy. When called with no collection, returns a transducer."
  ([pred]
   (with-meta (filter (complement pred))
     {:mino.fusion/stage {:kind :remove :f pred}}))
  ([pred coll] (lazy-remove pred coll)))
(defn vec
  "Converts coll into a vector. Coll must be nil, a sequential
   collection, a string, a map, a set, or a host array. Booleans,
   numbers, keywords, characters, regexes, and transients throw
   (matching JVM Clojure's `vec` rejecting non-seqable scalars)."
  [coll]
  (cond
    (nil? coll)         []
    (vector? coll)      coll
    (or (number? coll) (boolean? coll) (char? coll) (keyword? coll)
        (symbol? coll)  (regex? coll))
    (throw (str "vec: cannot create a vector from " (type coll)))
    (transient? coll)
    (throw (str "vec: cannot create a vector from a transient"))
    :else (into [] coll)))

(defn seq-to-map-for-destructuring
  "Builds a map from a sequence of keyword/value pairs, possibly with
   a trailing override map. Used by JVM Clojure's 1.11+ map-destructure
   over a varargs seq. Public so portable code can call it directly."
  [s]
  (let [s (seq s)]
    (cond
      (nil? s)
      {}
      (nil? (next s))
      (if (map? (first s)) (first s) {})
      :else
      (let [items (vec s)
            trailing-map? (map? (peek items))
            pair-items    (if trailing-map? (pop items) items)
            base          (apply hash-map pair-items)]
        (if trailing-map?
          (merge base (peek items))
          base)))))

;; --- Random utilities ---

(defn rand-int
  "Returns a random integer between 0 (inclusive) and n (exclusive)."
  [n] (int (* (rand) n)))
(defn rand-nth
  "Returns a random element from coll."
  [coll]
  (cond
    (nil? coll) nil
    (or (string? coll) (sequential? coll) (set? coll) (map? coll))
      (nth (vec coll) (rand-int (count coll)))
    :else
      (throw (str "rand-nth: expected a collection, got " (pr-str coll)))))
(defn random-sample
  "Returns items from coll with probability prob. When called with no
   collection, returns a transducer."
  ([prob]
   (filter (fn [_] (< (rand) prob))))
  ([prob coll]
   (filter (fn [_] (< (rand) prob)) coll)))

;; --- Side-effecting traversal ---

(defn run!
  "Applies f to each item in coll for side effects. Returns nil."
  [proc coll]
  (let [go (fn go [s] (when (seq s) (proc (first s)) (go (rest s))))]
    (go coll)
    nil))

;; --- Control flow macros ---

(defmacro if-not
  "Evaluates then when test is falsy, else otherwise."
  [test then & else]
  (if (seq else)
    `(if (not ~test) ~then ~(first else))
    `(if (not ~test) ~then)))

(defmacro when-not "Evaluates body when test is falsy." [test & body]
  `(when (not ~test) ~@body))

(defmacro if-let
  "Binds the result of expr, evaluates then if truthy, else otherwise."
  [bindings then & else]
  (when-not (and (vector? bindings) (= 2 (count bindings)))
    (throw "if-let requires a binding vector of exactly one symbol/expr pair"))
  (let [sym  (first bindings)
        expr (first (rest bindings))
        g    (gensym)]
    (if (seq else)
      `(let [~g ~expr]
         (if ~g (let [~sym ~g] ~then) ~(first else)))
      `(let [~g ~expr]
         (if ~g (let [~sym ~g] ~then))))))

(defmacro when-let
  "Binds the result of expr, evaluates body if truthy."
  [bindings & body]
  (when-not (and (vector? bindings) (= 2 (count bindings)))
    (throw "when-let requires a binding vector of exactly one symbol/expr pair"))
  (let [sym  (first bindings)
        expr (first (rest bindings))
        g    (gensym)]
    `(let [~g ~expr]
       (when ~g (let [~sym ~g] ~@body)))))

(defmacro when-first
  "Binds the first element of a collection, evaluates body if the
   collection is non-empty."
  [[x coll] & body]
  `(when-let [s# (seq ~coll)]
     (let [~x (first s#)] ~@body)))

(defmacro letfn
  "Binds local functions. Each binding is (name [params] body...).
   Expands to the `letfn*` special form so the bound fns can refer
   to each other (mutual recursion) — every name is placeholder-
   bound before any fn body is evaluated, so each fn's closure
   captures the shared scope."
  [fnspecs & body]
  (let [pairs (vec (mapcat
                     (fn [b]
                       [(first b)
                        (apply list 'fn (first b) (rest b))])
                     fnspecs))]
    `(letfn* ~pairs ~@body)))

(defmacro set!
  "Mutates a thread-local dynamic-var binding to the given value.
   The target must be a dynamic var with an enclosing (binding ...)
   form on the call stack; without one, throws \"Can't
   change/establish root binding\". Matches Clojure JVM's contract
   for set! on Vars. Returns the new value.

   The JVM-only field-mutation shape (set! (.-field obj) val) is not
   supported -- mino has no JVM fields."
  [target value]
  (when-not (symbol? target)
    (throw "set!: first argument must be a symbol naming a dynamic var"))
  (list 'set-dyn-binding! (list 'quote target) value))

(defmacro comment "Ignores body, returns nil." [& body] nil)

(defmacro if-some
  "Binds the result of expr, evaluates then if non-nil, else
   otherwise."
  [bindings then & else]
  (when-not (and (vector? bindings) (= 2 (count bindings)))
    (throw "if-some requires a binding vector of exactly one symbol/expr pair"))
  (let [sym  (first bindings)
        expr (first (rest bindings))
        g    (gensym)]
    (if (seq else)
      `(let [~g ~expr]
         (if (not (nil? ~g)) (let [~sym ~g] ~then) ~(first else)))
      `(let [~g ~expr]
         (if (not (nil? ~g)) (let [~sym ~g] ~then))))))

(defmacro when-some
  "Binds the result of expr, evaluates body if non-nil."
  [bindings & body]
  (when-not (and (vector? bindings) (= 2 (count bindings)))
    (throw "when-some requires a binding vector of exactly one symbol/expr pair"))
  (let [sym  (first bindings)
        expr (first (rest bindings))
        g    (gensym)]
    `(let [~g ~expr]
       (when (not (nil? ~g)) (let [~sym ~g] ~@body)))))

;; --- Sequence functions ---

;; -lock-* are private aliases captured at boot. Internal calls to
;; the underlying primitive route through these locals rather than
;; the public Var, so `(with-redefs [first last] ...)` and similar
;; tricks don't reroute the core fn's own recursion through the
;; redef and spin forever. JVM Clojure achieves the same effect
;; through direct-linking; mino has no such mechanism so we capture
;; per use site.
(def ^:private -lock-first first)
(def ^:private -lock-next  next)

(defn last "Returns the last item in coll." [coll]
  (let [s (seq coll)]
    (loop [s s]
      (if (-lock-next s)
        (recur (-lock-next s))
        (-lock-first s)))))

(defn butlast "Returns a seq of all but the last item in coll." [coll]
  (let [s (seq coll)]
    (when (next s)
      (cons (first s) (butlast (next s))))))

(defn nthrest "Returns the result of calling rest n times on coll." [coll n]
  (if (<= n 0) coll (nthrest (rest coll) (- n 1))))

(defn nthnext "Returns the result of calling next n times on coll." [coll n]
  (cond
    (nil? coll)         nil
    (not (integer? n))  (throw (str "nthnext: n must be an integer, got " (pr-str n)))
    (<= n 0)            (seq coll)
    :else               (nthnext (next coll) (- n 1))))

(defn take-last "Returns a seq of the last n items in coll." [n coll]
  (let [lead (drop n coll)
        step (fn step [s lead]
               (if (seq lead)
                 (step (next s) (next lead))
                 s))]
    (step (seq coll) (seq lead))))

(defn drop-last "Returns a lazy sequence of all but the last n items in coll."
  ([coll]   (drop-last 1 coll))
  ([n coll] (map (fn [x _] x) coll (drop n coll))))

(defn split-at "Returns a vector of [(take n coll) (drop n coll)]." [n coll]
  (vector (take n coll) (drop n coll)))

(defn split-with
  "Returns a vector of [(take-while pred coll) (drop-while pred coll)]."
  [pred coll]
  (vector (take-while pred coll) (drop-while pred coll)))

;; mapv is defined as a C primitive; no mino-level fallback needed.
;; filterv is defined as a C primitive; no mino-level fallback needed.

(defn sort-by
  "Returns a sorted sequence of the items in coll, ordered by (keyfn item)."
  ([keyfn coll]
   (sort (fn [a b] (compare (keyfn a) (keyfn b))) coll))
  ([keyfn comp coll]
   (sort (fn [a b] (comp (keyfn a) (keyfn b))) coll)))

;; --- Collection utilities ---

(def ^:private get-in-step
  (fn get-in-step [m ks not-found sentinel]
    (if ks
      (let [v (get m (first ks) sentinel)]
        (if (= v sentinel)
          not-found
          (get-in-step v (next ks) not-found sentinel)))
      m)))

(defn get-in
  "Returns the value in a nested associative structure at the given
   key path."
  ([m ks] (reduce get m ks))
  ([m ks not-found] (get-in-step m (seq ks) not-found (gensym))))

(defn assoc-in
  "Associates a value in a nested associative structure at the given
   key path."
  [m ks v]
  (let [k (first ks)]
    (if (next ks)
      (assoc m k (assoc-in (get m k) (rest ks) v))
      (assoc m k v))))

(defn update-in
  "Updates a value in a nested associative structure by applying f at
   the given key path."
  [m ks f & args]
  (let [k (first ks)]
    (if (next ks)
      (assoc m k (apply update-in (get m k) (rest ks) f args))
      (assoc m k (apply f (get m k) args)))))

;; merge-with is registered as a C primitive (see src/prim/core/sequences.c).

(defn reduce-kv
  "Reduces a map with f taking accumulator, key, and value."
  [f init coll]
  (reduce (fn [acc kv] (f acc (first kv) (second kv)))
          init (seq coll)))

(defn update-vals "Returns a map with f applied to each value." [m f]
  (reduce-kv (fn [acc k v] (assoc acc k (f v))) {} m))

(defn update-keys "Returns a map with f applied to each key." [m f]
  (reduce-kv (fn [acc k v] (assoc acc (f k) v)) {} m))

(defn replace
  "Returns a collection with items in coll replaced by entries in
   smap. When called with no collection, returns a transducer."
  ([smap]
   (map (fn [x] (if-let [e (find smap x)] (val e) x))))
  ([smap coll]
   (let [f (fn [x] (if-let [e (find smap x)] (val e) x))]
     (if (vector? coll)
       (with-meta (mapv f coll) (meta coll))
       (map f coll)))))

;; str-replace is now a C primitive in prim/string.c

;; --- Bitwise compositions ---

(defn bit-and-not
  "Returns the bitwise AND of x and the complement of each remaining
   argument, folded left to right."
  ([x] x)
  ([x y] (bit-and x (bit-not y)))
  ([x y & more] (reduce bit-and-not (bit-and-not x y) more)))
(defn bit-test
  "Returns true if bit n of x is set."
  [x n] (not= 0 (bit-and x (bit-shift-left 1 n))))
(defn bit-set
  "Returns x with bit n set."
  [x n] (bit-or x (bit-shift-left 1 n)))
(defn bit-clear
  "Returns x with bit n cleared."
  [x n] (bit-and x (bit-not (bit-shift-left 1 n))))
(defn bit-flip
  "Returns x with bit n flipped."
  [x n] (bit-xor x (bit-shift-left 1 n)))

;; --- Misc utilities ---

(defn comparator
  "Returns a comparator function from a two-arg predicate."
  [pred]
  (fn [a b] (cond (pred a b) -1 (pred b a) 1 :else 0)))

;; --- Lazy combinators ---

(defn keep
  "Returns a lazy sequence of non-nil results of (f item). When called
   with no collection, returns a transducer."
  ([f]
   (with-meta
     (fn [rf]
       (fn ([] (rf))
           ([result] (rf result))
           ([result input]
            (let [v (f input)]
              (if (nil? v)
                result
                (rf result v))))))
     {:mino.fusion/stage {:kind :keep :f f}}))
  ([f coll]
   (lazy-keep f coll)))

(defn keep-indexed
  "Returns a lazy sequence of non-nil results of (f index item).
   When called with no collection, returns a transducer."
  ([f]
   (fn [rf]
     (let [i (volatile! -1)]
       (fn ([] (rf))
           ([result] (rf result))
           ([result input]
            (let [v (f (vswap! i inc) input)]
              (if (nil? v) result (rf result v))))))))
  ([f coll]
   (let [keepi (fn keepi [i coll]
                 (lazy-seq
                   (when-let [s (seq coll)]
                     (if (chunked-seq? s)
                       (let [c (chunk-first s)
                             size (count c)
                             b (chunk-buffer size)]
                         (loop [j 0]
                           (when (< j size)
                             (let [v (f (+ i j) (nth c j))]
                               (when-not (nil? v) (chunk-append b v)))
                             (recur (inc j))))
                         (chunk-cons (chunk b)
                                     (keepi (+ i size) (chunk-rest s))))
                       ((fn [i coll]
                          (when-let [s (seq coll)]
                            (let [v (f i (first s))]
                              (if (nil? v)
                                (recur (inc i) (rest s))
                                (cons v (keepi (inc i) (rest s)))))))
                        i coll)))))]
     (keepi 0 coll))))

(defn map-indexed
  "Returns a lazy sequence of (f index item) for each item in coll.
   When called with no collection, returns a transducer."
  ([f]
   (with-meta
     (fn [rf]
       (let [i (volatile! -1)]
         (fn ([] (rf))
             ([result] (rf result))
             ([result input]
              (rf result (f (vswap! i inc) input))))))
     {:mino.fusion/stage {:kind :map-indexed :f f}}))
  ([f coll]
   (lazy-map-indexed f coll)))

(defn partition-all
  "Like partition, but includes a final partial group if items remain.
   The transducer arity emits each group as a vector (matching JVM
   Clojure's `(vec (.toArray buf))`); the seq arities emit lists per
   `partition`'s shape."
  ([n]
   (fn [rf]
     (let [buf (volatile! [])]
       (fn ([] (rf))
           ([result]
            (let [b @buf]
              (if (empty? b)
                (rf result)
                (let [r (rf result b)]
                  (rf (unreduced r))))))
           ([result input]
            (let [b (vswap! buf conj input)]
              (if (= (count b) n)
                (do (vreset! buf [])
                    (rf result b))
                result)))))))
  ([n coll] (partition-all n n coll))
  ([n step coll]
   (letfn [(pa-impl [n step coll]
             (lazy-seq
               (let [s (seq coll)]
                 (when s
                   (let [p (doall (take n s))]
                     (cons p (pa-impl n step (drop step s))))))))]
     (pa-impl n step coll))))

(defn reductions
  "Returns a lazy sequence of the intermediate values of a reduction.
   A reduced value from f (or as init) short-circuits: its dereferenced
   value becomes the final element."
  ([f coll]
   (lazy-seq
     (if-let [s (seq coll)]
       (reductions f (first s) (rest s))
       (list (f)))))
  ([f init coll]
   (if (reduced? init)
     (list @init)
     (cons init
           (lazy-seq
             (when-let [s (seq coll)]
               (reductions f (f init (first s)) (rest s))))))))

(defn dedupe
  "Returns a lazy sequence removing consecutive duplicates. When
   called with no collection, returns a transducer."
  ([]
   (fn [rf]
     (let [prev (volatile! ::none)]
       (fn ([] (rf))
           ([result] (rf result))
           ([result input]
            (if (= @prev input)
              result
              (do (vreset! prev input)
                  (rf result input))))))))
  ([coll] (sequence (dedupe) coll)))

;; --- Higher-order combinators ---

(defn every-pred
  "Returns a function that returns true when all preds are satisfied
   by all its arguments."
  ([p]
   (fn ep1
     ([] true)
     ([x] (boolean (p x)))
     ([x y] (boolean (and (p x) (p y))))
     ([x y z] (boolean (and (p x) (p y) (p z))))
     ([x y z & args] (boolean (and (ep1 x y z) (every? p args))))))
  ([p1 p2]
   (fn ep2
     ([] true)
     ([x] (boolean (and (p1 x) (p2 x))))
     ([x y] (boolean (and (p1 x) (p1 y) (p2 x) (p2 y))))
     ([x y z]
      (boolean (and (p1 x) (p1 y) (p1 z) (p2 x) (p2 y) (p2 z))))
     ([x y z & args]
      (boolean (and (ep2 x y z)
                    (every? (fn [a] (and (p1 a) (p2 a))) args))))))
  ([p1 p2 p3]
   (fn ep3
     ([] true)
     ([x] (boolean (and (p1 x) (p2 x) (p3 x))))
     ([x y]
      (boolean (and (p1 x) (p1 y) (p2 x) (p2 y) (p3 x) (p3 y))))
     ([x y z]
      (boolean (and (p1 x) (p1 y) (p1 z)
                    (p2 x) (p2 y) (p2 z)
                    (p3 x) (p3 y) (p3 z))))
     ([x y z & args]
      (boolean (and (ep3 x y z)
                    (every? (fn [a] (and (p1 a) (p2 a) (p3 a)))
                            args))))))
  ([p1 p2 p3 & ps]
   (let [ps (cons p1 (cons p2 (cons p3 ps)))]
     (fn epn
       ([] true)
       ([x] (every? (fn [p] (p x)) ps))
       ([x y] (every? (fn [p] (and (p x) (p y))) ps))
       ([x y z] (every? (fn [p] (and (p x) (p y) (p z))) ps))
       ([x y z & args] (boolean (and (epn x y z)
                                     (every? (fn [p] (every? p args)) ps))))))))

(defn some-fn
  "Returns a function that returns the first truthy value from any
   pred applied to any argument."
  ([p]
   (fn sp1
     ([] nil)
     ([x] (p x))
     ([x y] (or (p x) (p y)))
     ([x y z] (or (p x) (p y) (p z)))
     ([x y z & args] (or (sp1 x y z) (some p args)))))
  ([p1 p2]
   (fn sp2
     ([] nil)
     ([x] (or (p1 x) (p2 x)))
     ([x y] (or (p1 x) (p1 y) (p2 x) (p2 y)))
     ([x y z] (or (p1 x) (p1 y) (p1 z) (p2 x) (p2 y) (p2 z)))
     ([x y z & args] (or (sp2 x y z)
                         (some (fn [a] (or (p1 a) (p2 a))) args)))))
  ([p1 p2 p3]
   (fn sp3
     ([] nil)
     ([x] (or (p1 x) (p2 x) (p3 x)))
     ([x y] (or (p1 x) (p1 y) (p2 x) (p2 y) (p3 x) (p3 y)))
     ([x y z] (or (p1 x) (p1 y) (p1 z)
                  (p2 x) (p2 y) (p2 z)
                  (p3 x) (p3 y) (p3 z)))
     ([x y z & args] (or (sp3 x y z)
                         (some (fn [a] (or (p1 a) (p2 a) (p3 a))) args)))))
  ([p1 p2 p3 & ps]
   (let [ps (cons p1 (cons p2 (cons p3 ps)))]
     (fn spn
       ([] nil)
       ([x] (some (fn [p] (p x)) ps))
       ([x y] (some (fn [p] (or (p x) (p y))) ps))
       ([x y z] (some (fn [p] (or (p x) (p y) (p z))) ps))
       ([x y z & args] (or (spn x y z)
                           (some (fn [p] (some p args)) ps)))))))

(defn fnil
  "Returns a function like f, but replaces nil arguments with the
   given defaults."
  ([f x]
   (fn [a & args]
     (apply f (if (nil? a) x a) args)))
  ([f x y]
   (fn [a b & args]
     (apply f (if (nil? a) x a) (if (nil? b) y b) args)))
  ([f x y z]
   (fn [a b c & args]
     (apply f (if (nil? a) x a) (if (nil? b) y b)
            (if (nil? c) z c) args))))

(defn memoize
  "Returns a memoized version of f that caches return values by
   arguments."
  [f]
  (let [cache (atom {})]
    (fn [& args]
      (let [e (find (deref cache) args)]
        (if e
          (val e)
          (let [v (apply f args)]
            (swap! cache assoc args v)
            v))))))

(defn trampoline
  "Calls f with args, then repeatedly calls the result if it is a
   function."
  [f & args]
  (let [result (apply f args)
        bounce (fn bounce [r] (if (fn? r) (bounce (r)) r))]
    (bounce result)))

;; --- Threading macros ---

(defmacro as->
  "Binds expr to sym, then threads it through each form where sym can
   appear anywhere."
  [expr name & forms]
  (if (= 0 (count forms))
    expr
    `(let [~name ~expr]
       (as-> ~(first forms) ~name ~@(rest forms)))))

(defmacro cond->
  "Thread-first through forms whose tests are truthy."
  [expr & clauses]
  (if (< (count clauses) 2)
    expr
    (let [g    (gensym)
          test (first clauses)
          step (first (rest clauses))]
      `(let [~g ~expr]
         (cond-> (if ~test
                   ~(if (cons? step)
                      `(~(first step) ~g ~@(rest step))
                      `(~step ~g))
                   ~g)
                 ~@(rest (rest clauses)))))))

(defmacro cond->>
  "Thread-last through forms whose tests are truthy."
  [expr & clauses]
  (if (< (count clauses) 2)
    expr
    (let [g    (gensym)
          test (first clauses)
          step (first (rest clauses))]
      `(let [~g ~expr]
         (cond->> (if ~test
                    ~(if (cons? step)
                       `(~(first step) ~@(rest step) ~g)
                       `(~step ~g))
                    ~g)
                  ~@(rest (rest clauses)))))))

(defmacro some->
  "Thread-first through forms, short-circuiting on nil."
  [expr & forms]
  (if (= 0 (count forms))
    expr
    (let [g (gensym)]
      `(let [~g ~expr]
         (if (nil? ~g)
           nil
           (some-> (-> ~g ~(first forms)) ~@(rest forms)))))))

(defmacro some->>
  "Thread-last through forms, short-circuiting on nil."
  [expr & forms]
  (if (= 0 (count forms))
    expr
    (let [g (gensym)]
      `(let [~g ~expr]
         (if (nil? ~g)
           nil
           (some->> (->> ~g ~(first forms)) ~@(rest forms)))))))

;; --- Iteration macros ---

(defmacro doto
  "Evaluates x, then calls each form with x as the first argument.
   Returns x."
  [x & forms]
  (let [g       (gensym)
        stmts   (apply list (map (fn [f] (if (cons? f)
                                            `(~(first f) ~g ~@(rest f))
                                            `(~f ~g)))
                                  forms))]
    `(let [~g ~x]
       ~@stmts
       ~g)))

(defmacro dotimes
  "Evaluates body n times with sym bound to 0 through n-1."
  [bindings & body]
  (let [sym (first bindings)
        n   (first (rest bindings))
        gn  (gensym)
        go  (gensym)]
    ;; Named fn so the body can self-reference; mino's let is
    ;; sequential (init exprs do not see their own binding) per
    ;; Clojure semantics, so a plain `(let [go (fn [...] (go))])`
    ;; would leave (go) unbound at fn-body evaluation.
    `(let [~gn ~n
           ~go (fn ~go [~sym]
                 (when (< ~sym ~gn)
                   ~@body
                   (~go (inc ~sym))))]
       (~go 0))))

(defmacro while
  "Repeatedly evaluates body while test is truthy."
  [test & body]
  (let [go (gensym)]
    `(let [~go (fn ~go [] (when ~test ~@body (~go)))]
       (~go))))

(defmacro doseq
  "Iterates over collections for side effects, evaluating body once
   per binding combination, and returns nil. Supports nested
   bindings and the modifier clauses :let, :when, and :while -- the
   same surface clojure.core/doseq exposes:
     :let [name expr ...]  introduces locals visible to inner clauses
     :when expr            skips this iteration when expr is falsy
     :while expr           halts all iteration when expr is falsy

  Implementation note: :while needs to stop the outer loop too, not
  just the inner one. We encode that with a shared 'stop' atom that
  the outer driver inspects each iteration. Without it, an outer
  infinite seq paired with a later :while would never terminate."
  [seq-exprs & body]
  (let [stop-sym (gensym "doseq-stop_")]
    (letfn [(emit [seq-exprs]
              (cond
                (zero? (count seq-exprs))
                `(do ~@body nil)

                (= :let (first seq-exprs))
                (let [bs            (first (rest seq-exprs))
                      rest-bindings (into [] (drop 2 seq-exprs))]
                  `(let ~bs ~(emit rest-bindings)))

                (= :when (first seq-exprs))
                (let [pred          (first (rest seq-exprs))
                      rest-bindings (into [] (drop 2 seq-exprs))]
                  `(when ~pred ~(emit rest-bindings)))

                (= :while (first seq-exprs))
                (let [pred          (first (rest seq-exprs))
                      rest-bindings (into [] (drop 2 seq-exprs))]
                  `(if ~pred
                     ~(emit rest-bindings)
                     (do (reset! ~stop-sym true) nil)))

                ;; Plain binding sym/coll. Drive a recursive loop.
                :else
                (let [sym           (first seq-exprs)
                      coll          (first (rest seq-exprs))
                      rest-bindings (into [] (drop 2 seq-exprs))
                      gs            (gensym)
                      go            (gensym)]
                  ;; Use a named fn so the body can self-reference;
                  ;; mino's `let` follows Clojure's sequential
                  ;; semantics where init exprs do not see their own
                  ;; binding, so a plain `(let [go (fn [...] (go))])`
                  ;; would leave (go) unbound at fn-body evaluation.
                  `(let [~go (fn ~go [~gs]
                               (when (and ~gs (not @~stop-sym))
                                 (let [~sym (first ~gs)]
                                   ~(emit rest-bindings)
                                   (~go (next ~gs)))))]
                     (~go (seq ~coll))))))]
      `(let [~stop-sym (atom false)]
         ~(emit seq-exprs)
         nil))))

;; --- Shuffle (Fisher-Yates) ---

(defn shuffle
  "Returns a randomly shuffled vector of the items in coll."
  [coll]
  (when-not (coll? coll) (throw "shuffle requires a collection"))
  (let [v   (vec coll)
        n   (count v)
        step (fn step [v i]
               (if (= i 0)
                 v
                 (let [j (rand-int (inc i))]
                   (step (assoc (assoc v i (nth v j)) j (nth v i))
                         (dec i)))))]
    (into [] (step v (dec n)))))

;; --- Timing ---

(defmacro time
  "Evaluates body, prints elapsed time, and returns the result."
  [& body]
  (let [start  (gensym)
        result (gensym)]
    `(let [~start  (time-ms)
           ~result (do ~@body)]
       (println (str "Elapsed time: " (- (time-ms) ~start) " ms"))
       ~result)))

;; --- Tree walking ---

(defn sequential?
  "Returns true if x is a sequential collection (list, vector,
   lazy-seq, or queue)."
  [coll]
  (or (cons? coll) (vector? coll) (seq? coll) (= :queue (type coll))))

(defn flatten
  "Returns a lazy sequence of the non-sequential items from a nested
   structure."
  [x]
  (filter (complement sequential?)
          (rest (tree-seq sequential? seq x))))

(defn tree-seq
  "Returns a lazy depth-first sequence of nodes in a tree."
  [branch? children root]
  (let [walk (fn walk [node]
               (lazy-seq
                 (cons node
                   (when (branch? node)
                     (mapcat walk (children node))))))]
    (walk root)))

(defn xml-seq
  "A tree seq on the xml elements as per xml/parse: nodes are maps
   with :tag and :content keys, leaves are strings."
  [root]
  (tree-seq (complement string?) (comp seq :content) root))

(defn walk
  "Traverses form, applying inner to each element and outer to the
   result."
  [inner outer form]
  (cond
    (vector? form) (outer (mapv inner form))
    (map?    form) (outer (into (empty form) (map inner (seq form))))
    (set?    form) (outer (into (empty form) (map inner form)))
    (seq?    form) (outer (apply list (map inner form)))
    true           (outer form)))

(defn postwalk
  "Walks form depth-first, applying f to each sub-form after its
   children."
  [f form] (walk (fn [x] (postwalk f x)) f form))
(defn prewalk
  "Walks form depth-first, applying f to each sub-form before its
   children."
  [f form] (walk (fn [x] (prewalk f x)) identity (f form)))

(defn postwalk-replace
  "Replaces items in form that appear as keys in smap, walking
   bottom-up."
  [smap form]
  (postwalk (fn [x] (if (contains? smap x) (get smap x) x)) form))

(defn prewalk-replace
  "Replaces items in form that appear as keys in smap, walking
   top-down."
  [smap form]
  (prewalk (fn [x] (if (contains? smap x) (get smap x) x)) form))

;; --- Regex ---
;;
;; Gated on the :regex capability. When the host did not call
;; mino_install_regex (or mino_install_clojure_core / mino_install_all),
;; the entire section is skipped: re-pattern / re-find / re-matches are
;; not bound, so any code calling them triggers a MNS002 "capability
;; not installed" diagnostic instead of a bare unbound-symbol error.

(when (mino-installed? :regex)

;; re-pattern is a C primitive that returns a MINO_REGEX from a
;; string source (and a no-op on an existing regex). Note that
;; clojure.string/split also accepts both regex and string patterns.

;; Capture the C primitives before the matcher-aware wrappers below
;; shadow them.
(def ^:private prim-re-find    re-find)
(def ^:private prim-re-matches re-matches)

(defn- re-require-pattern
  "Guard for the re match fns: the pattern argument must be a compiled
   regex, so metachar intent stays explicit. Strings are rejected;
   build a pattern with re-pattern or a regex literal."
  [fname re]
  (when-not (regex? re)
    (throw {:mino/kind :eval/type :mino/code "MTY001"
            :mino/message (str fname ": pattern must be a compiled regex;"
                               " build one with re-pattern")
            :mino/data {:got re}})))

(defn re-seq
  "Returns a lazy sequence of all matches of pattern in string s. Each
   match is a string when the pattern has no groups, or a vector
   [whole g1 g2 ...] when it does."
  [re s]
  (re-require-pattern "re-seq" re)
  (letfn [(step [pos]
            (lazy-seq
              (when-let [[m start end] (re-find-from re s pos)]
                ;; A zero-width match advances the scan one byte so
                ;; the walk terminates.
                (cons m (step (if (= start end) (inc end) end))))))]
    ;; Canon finds the first match eagerly and returns nil when the
    ;; pattern never matches; only the tail stays lazy.
    (seq (step 0))))

;; re-matcher: a stateful matcher value backed by an atom holding
;; {:pattern :text :pos :last}. Each (re-find m) advances :pos past the
;; previous match's end and stores the latest result under :last.
(defn re-matcher
  "Returns a matcher value for repeated find/match operations on text
   using pattern. The resulting value is consumed by re-find, re-groups
   and so on."
  [re s]
  (re-require-pattern "re-matcher" re)
  (atom {::matcher? true :pattern re :text s :pos 0 :last nil}))

(defn ^:private matcher? [m]
  (and (atom? m)
       (let [v (deref m)]
         (and (map? v) (::matcher? v)))))

(defn- re-find-on-matcher [m]
  (let [state   @m
        pattern (:pattern state)
        text    (:text state)
        pos     (:pos state)]
    (when-let [[result start end] (re-find-from pattern text pos)]
      ;; A zero-width match advances :pos one byte so repeated
      ;; finds terminate.
      (swap! m assoc :pos (if (= start end) (inc end) end) :last result)
      result)))

(defn re-find
  "Find the first match. (re-find pattern text) returns a string (no
   groups) or [whole g1 g2 ...] (groups). (re-find m) advances a matcher."
  ([m]            (re-find-on-matcher m))
  ([re s]
   (re-require-pattern "re-find" re)
   (prim-re-find re s)))

(defn re-matches
  "Like re-find but anchored to the whole string. Returns a string
   (no groups) or [whole g1 g2 ...] (groups), or nil."
  [re s]
  (re-require-pattern "re-matches" re)
  (prim-re-matches re s))

(defn re-groups
  "Returns the most recent match groups for matcher m: a vector
   [whole g1 g2 ...] when the pattern has groups, the whole-match string
   otherwise. Throws when the matcher has no recorded match yet."
  [m]
  (let [last-match (:last @m)]
    (when (nil? last-match)
      (throw (ex-info "No match has been performed by the matcher"
                      {:matcher m})))
    last-match))

) ;; end (when (mino-installed? :regex) ...)

;; --- Complex macros ---

(defmacro condp
  "Takes a binary predicate, an expression, and clauses. Returns the
   first clause value where (pred test-val expr) is truthy. The
   special clause shape `test :>> result-fn` calls `(result-fn p)`
   on the truthy pred-result whenever `(pred test expr)` is truthy."
  [pred expr & clauses]
  (let [gpred (gensym "pred__")
        gexpr (gensym "expr__")
        build (fn build [cls]
                (cond
                  (empty? cls) nil
                  (= (count cls) 1) (first cls)
                  (and (>= (count cls) 3) (= (second cls) :>>))
                  (let [test   (first cls)
                        res-fn (nth cls 2)
                        more   (drop 3 cls)
                        gp     (gensym "p__")]
                    `(if-let [~gp (~gpred ~test ~gexpr)]
                       (~res-fn ~gp)
                       ~(build more)))
                  :else
                  (let [test (first cls)
                        then (second cls)
                        more (drop 2 cls)]
                    `(if (~gpred ~test ~gexpr)
                       ~then
                       ~(build more)))))]
    `(let [~gpred ~pred ~gexpr ~expr]
       ~(build clauses))))

(defmacro case
  "Dispatches on the value of expr. Matches constants in pairs, with
   an optional default."
  [e & clauses]
  (let [gexpr   (gensym)
        quote-c (fn [c]
                  (cond
                    (nil? c)     nil
                    (keyword? c) c
                    (number? c)  c
                    (string? c)  c
                    (= c true)  true
                    (= c false) false
                    :else        (list 'quote c)))
        match1  (fn [g c]
                  (if (cons? c)
                    ;; Multi-match list: (a b c) => (or (= g 'a) (= g 'b) ...)
                    (apply list 'or (map (fn [v] (list '= g (quote-c v))) c))
                    (list '= g (quote-c c))))
        build   (fn build [cls]
                  (if (< (count cls) 2)
                    (if (= (count cls) 1)
                      (first cls)
                      (list 'throw (list 'ex-info "no matching case" {})))
                    (list 'if (match1 gexpr (first cls))
                          (first (rest cls))
                          (build (rest (rest cls))))))]
    `(let [~gexpr ~e]
       ~(build clauses))))

(defmacro for
  "List comprehension. Takes binding vectors and body, returns a lazy
   sequence."
  [bindings & body]
  (let [sym  (first bindings)
        coll (first (rest bindings))
        rest-bindings (drop 2 bindings)]
    (if (empty? rest-bindings)
      ;; Last binding pair: produce elements
      `(map (fn [~sym] ~@body) ~coll)
      (let [modifier (first rest-bindings)]
        (cond
          ;; :when filter
          (= modifier :when)
          (let [pred (first (rest rest-bindings))
                remaining (into [] (drop 2 rest-bindings))]
            (if (empty? remaining)
              `(map (fn [~sym] ~@body)
                    (filter (fn [~sym] ~pred) ~coll))
              `(for ~(into [sym (list 'filter (list 'fn [sym] pred) coll)]
                           remaining)
                 ~@body)))
          ;; :while stop iteration
          (= modifier :while)
          (let [pred (first (rest rest-bindings))
                remaining (into [] (drop 2 rest-bindings))]
            (if (empty? remaining)
              `(map (fn [~sym] ~@body)
                    (take-while (fn [~sym] ~pred) ~coll))
              `(for ~(into [sym (list 'take-while (list 'fn [sym] pred) coll)]
                           remaining)
                 ~@body)))
          ;; :let local bindings
          (= modifier :let)
          (let [let-bindings (first (rest rest-bindings))
                after-let (into [] (drop 2 rest-bindings))
                ;; Collect :when/:while modifiers that follow the :let
                next-mod (first after-let)]
            (cond
              (empty? after-let)
              `(map (fn [~sym]
                      (let ~let-bindings ~@body))
                    ~coll)
              ;; :when after :let on same binding
              (= next-mod :when)
              (let [pred (first (rest after-let))
                    remaining (into [] (drop 2 after-let))]
                (if (empty? remaining)
                  `(map (fn [~sym]
                          (let ~let-bindings ~@body))
                        (filter (fn [~sym]
                                  (let ~let-bindings ~pred))
                                ~coll))
                  `(mapcat (fn [~sym]
                             (let ~let-bindings
                               (when ~pred
                                 (for ~remaining ~@body))))
                           ~coll)))
              ;; :while after :let on same binding
              (= next-mod :while)
              (let [pred (first (rest after-let))
                    remaining (into [] (drop 2 after-let))]
                (if (empty? remaining)
                  `(map (fn [~sym]
                          (let ~let-bindings ~@body))
                        (take-while (fn [~sym]
                                      (let ~let-bindings ~pred))
                                    ~coll))
                  `(mapcat (fn [~sym]
                             (let ~let-bindings
                               (for ~remaining ~@body)))
                           (take-while (fn [~sym]
                                         (let ~let-bindings ~pred))
                                       ~coll))))
              ;; Other bindings after :let
              :else
              `(mapcat (fn [~sym]
                         (let ~let-bindings
                           (for ~after-let ~@body)))
                       ~coll)))
          ;; Another binding pair: nested iteration
          :else
          `(mapcat (fn [~sym]
                     (for ~(into [] rest-bindings) ~@body))
                   ~coll))))))

;; ---------------------------------------------------------------------------
;; Agents: not supported on mino.
;;
;; Real Clojure agents have asynchronous send semantics, an error-mode
;; lifecycle, and a thread-pool dispatcher. mino has none of those, so
;; rather than aliasing agent to atom and pretending the API works, the
;; functions throw with a clear message pointing at atoms or
;; core.async as the supported alternatives.
;; ---------------------------------------------------------------------------

;; ---------------------------------------------------------------------------
;; Exception info
;; ---------------------------------------------------------------------------

(defn ex-info
  "Create an exception map with a message and data map. The 3-arity
  form additionally attaches a cause; ex-cause walks the chain via
  metadata so the visible map structure stays the same as the
  2-arity form. The data argument must be a map (or nil)."
  ([msg map]
   (when-not (or (nil? map) (map? map))
     (throw {:mino/kind :type :mino/code "MTY001"
             :mino/message "ex-info: data must be a map"}))
   {:message msg :data map})
  ([msg map cause]
   (when-not (or (nil? map) (map? map))
     (throw {:mino/kind :type :mino/code "MTY001"
             :mino/message "ex-info: data must be a map"}))
   (with-meta {:message msg :data map} {:cause cause})))

(defn ex-data
  "Extract the data map from an exception. Handles diagnostic maps
  (from catch), ex-info maps, and plain thrown values."
  [ex]
  (when (map? ex)
    (if (:mino/kind ex)
      ;; Diagnostic map: unwrap to the original thrown value.
      ;; If that value is an ex-info map, extract its :data.
      (let [orig (:mino/data ex)]
        (if (and (map? orig) (contains? orig :data))
          (:data orig)
          orig))
      (:data ex))))

(defn ex-message
  "Extract the message from an exception. Handles both diagnostic maps
  and ex-info maps."
  [ex]
  (when (map? ex)
    (if (:mino/kind ex)
      (:mino/message ex)
      (:message ex))))

;; agent / send / send-off / await / agent-error / restart-agent /
;; set-error-handler! / error-handler / set-error-mode! / error-mode /
;; agent? / await-for / shutdown-agents / release-pending-sends are
;; provided by mino_install_agent (src/prim/agent/agent.c). mino's MVP runs
;; sends synchronously on the calling thread, so await is a no-op.
;; See /documentation/stm/ for the full deviation list.

;; ---------------------------------------------------------------------------
;; Host threads.
;;
;; Real OS-thread futures and promises. Embedded mode starts at
;; thread_limit = 1 (single-threaded) and embedders raise it via
;; mino_set_option(S, MINO_OPT_THREAD_LIMIT, n). Standalone `./mino`
;; grants cpu_count automatically so the REPL surface matches Clojure
;; canon.
;;
;; When the host has not granted threads (limit <= 1), spawn entry
;; points throw :mino/unsupported with a message naming the grant
;; API. When the host has granted, the C primitives behind these
;; functions create real pthread/CreateThread workers.
;; ---------------------------------------------------------------------------

(defn ^:private mino-no-grant-msg
  "Build the :mino/unsupported message used when host threads are
  not granted in the current state."
  [name]
  (str name ": host threads are not granted in this state. "
       "Embedders call mino_set_option(S, MINO_OPT_THREAD_LIMIT, n) "
       "with n > 1; the standalone `./mino` binary grants cpu_count "
       "automatically."))

(defmacro future
  "Takes a body of expressions and yields a future object that will
  evaluate the body in another thread, blocking on deref until the
  value is available. Throws :mino/unsupported when host threads are
  not granted; see mino-thread-limit."
  [& body]
  `(if (<= (mino-thread-limit) 1)
     (throw (ex-info (mino-no-grant-msg "future")
                     {:mino/unsupported :future
                      :mino/thread-limit (mino-thread-limit)}))
     (future-call (fn [] ~@body))))

(defmacro thread
  "Executes the body in another thread, returning a future-like value
  that can be deref'd. Shares the same worker pool as future. Throws
  :mino/unsupported when host threads are not granted."
  [& body]
  `(if (<= (mino-thread-limit) 1)
     (throw (ex-info (mino-no-grant-msg "thread")
                     {:mino/unsupported :thread
                      :mino/thread-limit (mino-thread-limit)}))
     (future-call (fn [] ~@body))))

(defn pmap
  "Like map, except f is applied in parallel via futures. Semi-lazy in
   that the parallel computation stays no more than thread-limit-1
   items ahead of the consumer. f's invocations across the collection
   are independent; do not pmap with a side-effectful f if order of
   effects matters.

   Single-arity collection only: (pmap f coll). When host threads are
   not granted (mino-thread-limit <= 1), falls back to (map f coll)
   so callers don't need a conditional."
  ([f coll]
   (if (<= (mino-thread-limit) 1)
     (map f coll)
     (let [chunk-size (max 1 (- (mino-thread-limit) 1))
           step (fn step [s]
                  (lazy-seq
                    (let [realized-chunk (doall (map (fn [x] (future (f x)))
                                                     (take chunk-size s)))
                          remaining       (drop chunk-size s)]
                      (if (seq remaining)
                        (concat (map deref realized-chunk) (step remaining))
                        (map deref realized-chunk)))))]
       (step coll)))))

(defn pcalls
  "Executes the no-arg fns in parallel, returning a lazy sequence of
   their values. Mirrors clojure.core/pcalls. When host threads aren't
   granted (mino-thread-limit <= 1), falls back to sequential map so
   the surface is portable across embedded and CLI runs."
  [& fns]
  (pmap (fn [f] (f)) fns))

(defmacro pvalues
  "Returns a lazy sequence of the values of the exprs, which are
   evaluated in parallel via pcalls. Mirrors clojure.core/pvalues."
  [& exprs]
  `(pcalls ~@(map (fn [e] `(fn [] ~e)) exprs)))

(defn seque
  "Returns a seq with the same elements and order as s, where a
   producer running on another thread stays up to n elements (default
   128) ahead of the consumer. The producer realizes s in n-sized
   batches, each batch filling while the consumer walks the previous
   one. When host threads are not granted (mino-thread-limit <= 1),
   falls back to (seq s) so callers don't need a conditional."
  ([s] (seque 128 s))
  ([n-or-q s]
   (when-not (and (int? n-or-q) (pos? n-or-q))
     (throw (ex-info "seque: buffer size must be a positive integer"
                     {:got n-or-q})))
   (if (<= (mino-thread-limit) 1)
     (seq s)
     (let [step (fn step [fut s]
                  (lazy-seq
                    (let [chunk (deref fut)]
                      (when (seq chunk)
                        (let [more (drop n-or-q s)
                              nfut (future-call
                                     (fn [] (doall (take n-or-q more))))]
                          (concat chunk (step nfut more)))))))]
       (step (future-call (fn [] (doall (take n-or-q s)))) s)))))

;; ---------------------------------------------------------------------------
;; Protocols: polymorphic dispatch on the type of the first argument.
;; Gated on the :protocols capability -- defprotocol / extend-protocol /
;; extend-type / satisfies? are absent when the host did not install
;; protocols. The CollReduce / IKVReduce / Datafiable / Navigable
;; extension points further down also live in this gate.
;;
;; (defprotocol Name
;;   (method1 [this])
;;   (method2 [this x]))
;;
;; (extend-type :string
;;   Name
;;   (method1 [this] ...)
;;   (method2 [this x] ...))
;;
;; (extend-protocol Name
;;   :string
;;   (method1 [this] ...)
;;   :vector
;;   (method1 [this] ...))
;;
;; (satisfies? Name x) returns true if x's type has implementations for Name.
;; ---------------------------------------------------------------------------

(when (mino-installed? :protocols)

(defn protocol-dispatch [dispatch-atom mname & args]
  (let [target (first args)
        t (type target)
        dispatch-map @dispatch-atom
        impl (get dispatch-map t)]
    (if impl
      (apply impl args)
      (let [default (get dispatch-map :default)]
        (if default
          (apply default args)
          (throw (ex-info (str "No implementation of " mname
                               " for type " (name t))
                          {:method mname :type t})))))))

(defmacro defprotocol
  "Defines a protocol with the given method signatures."
  [name & opts+sigs]
  (let [pname (clojure.core/name name)]
    (letfn [(method-meta [m]
              (let [mname (first m)
                    sigs  (vec (take-while vector? (rest m)))]
                {:mname mname
                 :sigs sigs
                 :dsym (symbol (str pname "--" (clojure.core/name mname)))}))
            (method-defn [mi]
              ;; Single-signature methods keep the exact single-arity
              ;; shape the BC compiler's protocol-IC recognizer keys
              ;; on; multi-signature methods emit an arity-dispatching
              ;; fn (correct, falls back to the generic call path).
              (if (= 1 (count (:sigs mi)))
                (let [params (first (:sigs mi))]
                  (list 'defn (:mname mi) params
                        (apply list 'protocol-dispatch
                               (:dsym mi)
                               (str (:mname mi))
                               params)))
                (apply list 'defn (:mname mi)
                       (map (fn [params]
                              (list params
                                    (apply list 'protocol-dispatch
                                           (:dsym mi)
                                           (str (:mname mi))
                                           params)))
                            (:sigs mi)))))]
      (let [methods     (remove string? opts+sigs)
            methods     (loop [ms methods result []]
                          (if (or (nil? ms) (empty? ms))
                            result
                            (if (keyword? (first ms))
                              (recur (drop 2 ms) result)
                              (recur (rest ms)
                                     (conj result (first ms))))))
            method-info (into [] (map method-meta methods))
            atom-defs   (into [] (map (fn [mi]
                                        (list 'def (:dsym mi)
                                              '(atom {})))
                                      method-info))
            fn-defs     (into [] (map method-defn method-info))
            proto-map   (into {} (map (fn [mi]
                                        [(keyword (str (:mname mi)))
                                         (:dsym mi)])
                                      method-info))
            proto-def   (list 'def name
                              {:name pname :methods proto-map})
            all-forms   (concat atom-defs fn-defs
                                (list proto-def))]
        (apply list 'do all-forms)))))

(defmacro extend-type
  "Extends a protocol with method implementations for the given type."
  [t & specs]
  (let [groups (loop [remaining specs
                      result []
                      cur-proto nil
                      cur-methods []]
                 (if (empty? remaining)
                   (if cur-proto
                     (conj result [cur-proto cur-methods])
                     result)
                   (let [item (first remaining)]
                     (if (and (symbol? item) (not (list? item)))
                       (recur (rest remaining)
                              (if cur-proto
                                (conj result [cur-proto cur-methods])
                                result)
                              item [])
                       (recur (rest remaining) result cur-proto
                              (conj cur-methods item))))))
        swaps (mapcat (fn [[proto methods]]
                (let [pname (name proto)
                      pns   (namespace proto)]
                  (map (fn [m]
                    (let [mname (first m)
                          tail  (rest m)]
                      (when-not (symbol? mname)
                        (throw (str "extend-type: method must start with a"
                                    " name symbol, got: " (pr-str m))))
                      ;; Single arity: (m [params] body...).
                      ;; Multi-arity: (m ([p] body...) ([p k] body...)).
                      (when-not (or (vector? (first tail))
                                    (and (seq tail)
                                         (every? (fn [clause]
                                                   (and (seq? clause)
                                                        (vector? (first clause))))
                                                 tail)))
                        (throw (str "extend-type: method " (pr-str mname)
                                    " must have a params vector or arity"
                                    " clauses, got: " (pr-str (first tail)))))
                      (let [dsym (symbol pns (str pname "--" (name mname)))
                            fn-form (apply list 'fn tail)]
                        (list 'swap! dsym 'assoc t fn-form))))
                   methods)))
              groups)]
    (apply list 'do (vec swaps))))

(defn- type-marker-key
  "Translates an extend-protocol type marker to the dispatch key
   extend-type understands. Keywords pass through (used for built-in
   types and ad-hoc tags via :type metadata). Symbols pass through
   too: at runtime they evaluate to a MINO_TYPE value so
   (extend-protocol P Point ...) places its impls under that type's
   pointer in the dispatch atom. Nil maps to :nil so a
   (extend-protocol P nil ...) clause registers against (type nil)."
  [marker]
  (cond
    (keyword? marker) marker
    (nil? marker)     :nil
    (symbol? marker)  marker
    :else
    (throw (ex-info
             (str "extend-protocol: unsupported type marker "
                  (pr-str marker)
                  " — use a keyword, a record-type symbol, or nil")
             {:marker marker
              :mino/unsupported :extend-protocol-type-marker}))))

(defn- partition-protocol-specs [specs]
  (loop [remaining specs groups [] current nil]
    (if (empty? remaining)
      (if current (conj groups current) groups)
      (let [item (first remaining)]
        (if (or (cons? item) (list? item))
          (do
            (when (nil? current)
              (throw (ex-info
                       (str "extend-protocol: method spec "
                            (pr-str item)
                            " is missing a preceding type marker. "
                            "A reader-conditional that elides its type "
                            "(for example #?(:clj Object) with no :default "
                            "branch under another dialect) can leave the "
                            "macro with method specs and nothing to extend "
                            "— add a :default branch so the type marker "
                            "survives, or supply one inline.")
                       {:method-spec item
                        :mino/unsupported
                        :extend-protocol-missing-type-marker})))
            (recur (rest remaining) groups (conj current item)))
          (recur (rest remaining)
                 (if current (conj groups current) groups)
                 [(type-marker-key item)]))))))

(defmacro extend-protocol
  "Extends a protocol with implementations for multiple types."
  [p & specs]
  (let [groups (partition-protocol-specs specs)
        forms (into [] (map (fn [group]
                              (apply list 'extend-type
                                     (first group) p
                                     (rest group)))
                            groups))]
    (apply list 'do forms)))

(defn satisfies?
  "Returns true if x's type has implementations for all methods of
   proto. proto must be a protocol; anything else throws."
  [protocol x]
  (when-not (and (map? protocol)
                 (string? (:name protocol))
                 (map? (:methods protocol)))
    (throw {:mino/kind :eval/type :mino/code "MTY001"
            :mino/message "satisfies?: first argument must be a protocol"
            :mino/data {:got protocol}}))
  (let [t (type x)
        methods (vals (:methods protocol))]
    (every? (fn [dispatch-atom]
              (let [dm @dispatch-atom]
                (or (contains? dm t)
                    (contains? dm :default))))
            methods)))

(defn extend
  "Registers method implementations for one or more protocols on type
   t without generating wrapper code: (extend T P {:m (fn [x] ...)}).
   The fn-map keys are keywordized method names; values are the
   implementation fns."
  [atype & proto+mmaps]
  (let [tk (if (nil? atype) :nil atype)]
    (doseq [[proto mmap] (partition 2 proto+mmaps)]
      (doseq [[mkw f] mmap]
        (let [a (get (:methods proto) mkw)]
          (when (nil? a)
            (throw (ex-info (str "extend: protocol " (:name proto)
                                 " has no method " mkw)
                            {:protocol (:name proto) :method mkw})))
          (swap! a assoc tk f)))))
  nil)

(defn extends?
  "Returns true if type t has been extended to proto (an explicit
   registration for at least one method; :default does not count)."
  [protocol atype]
  (boolean (some (fn [dispatch-atom] (contains? @dispatch-atom atype))
                 (vals (:methods protocol)))))

(defn extenders
  "Returns a seq of the types explicitly extended to proto, or nil
   when there are none."
  [protocol]
  (seq (distinct (remove (fn [t] (= t :default))
                         (mapcat (fn [a] (keys @a))
                                 (vals (:methods protocol)))))))

;; ---------------------------------------------------------------------------
;; Core protocols: extension points wired into reduce / reduce-kv /
;; datafy / nav.
;;
;; CollReduce  - per-type reduction strategy. The 3-arg form (coll-reduce
;;               coll f init) is the workhorse; user types extend this and
;;               reduce dispatches through it.
;; IKVReduce   - associative kv-reduce dispatch. Defaults walk seq pairs.
;; Datafiable  - per-type datafy hook; default returns the value unchanged.
;; Navigable   - per-coll/key/value nav hook; default returns v unchanged.
;;
;; Built-in collection types do not extend these; reduce / reduce-kv keep
;; the existing fast paths and consult the protocol only when a user has
;; registered a per-type override (or a :default catch-all).
;; ---------------------------------------------------------------------------

(def internal-reduce reduce)
(def internal-reduce-kv reduce-kv)

(defprotocol CollReduce
  "Protocol for collection types that can reduce themselves. User types
  extend coll-reduce to provide a custom traversal; mino's reduce
  consults the registry before falling back to its built-in seq-driven
  reduction."
  (coll-reduce [coll f init]))

(defprotocol IKVReduce
  "Protocol for associative collections that can kv-reduce directly,
  rather than walk a seq of [k v] pairs."
  (kv-reduce [coll f init]))

;; JVM Clojure's reduce-kv on a vector uses (index, element) pairs.
;; Provide that directly so `(reduce-kv f init [a b c])` calls
;; `(f acc 0 a)`, `(f acc 1 b)`, `(f acc 2 c)` rather than falling
;; through to internal-reduce-kv (which iterates over `(seq coll)`
;; and decomposes each element as a pair — yielding char-keyed
;; nonsense when the elements are strings).
(extend-type :vector IKVReduce
  (kv-reduce [coll f init]
    (let [n (count coll)]
      (loop [i 0 acc init]
        (if (>= i n)
          acc
          (let [r (f acc i (nth coll i))]
            (if (reduced? r) @r (recur (inc i) r))))))))

(defprotocol Datafiable
  "Protocol for things that can present a data view of themselves.
  The default impl is identity; user types may override."
  (datafy [o]))

(extend-type :default Datafiable (datafy [o] o))

(defprotocol Navigable
  "Protocol for navigating from a (k v) pair in coll into a related
  value. The default impl returns v unchanged."
  (nav [coll k v]))

(extend-type :default Navigable (nav [_coll _k v] v))

;; Unique marker for the no-init reduce path. A custom coll-reduce is
;; 3-arg (coll f init), so the 2-arg reduce emulates "first element is
;; the init" by handing the impl this sentinel and a wrapper f that
;; adopts the first element rather than combining it. The sentinel is
;; module-private and never escapes: the empty coll is handled before
;; the impl runs, so no reduction ever returns it.
(def ^:private reduce-no-init-sentinel (atom :reduce-no-init))

(defn reduce "Reduces coll using f. With 2 args, uses the first element as init.
  With 3 args, uses val explicitly. Consults CollReduce: a user
  type or :default override on coll-reduce takes precedence over the
  built-in seq-driven reduction."
    ([f coll]
     ;; Look up the protocol impl on the ORIGINAL coll's type. If
     ;; no user override exists, hand the coll through to the C
     ;; primitive as-is -- it has its own 2-arg fast paths (int
     ;; range, persistent vec/map/set) that rely on coll being
     ;; the unforced source value. Only route through the impl when a
     ;; user-extended CollReduce impl is taking over.
     (let [table @CollReduce--coll-reduce
           impl  (or (get table (type coll))
                     (get table :default))]
       (if impl
         ;; Dispatch to the impl over the ORIGINAL coll (like the 3-arg
         ;; arm), reproducing the no-init contract: empty -> (f); a
         ;; non-empty coll uses its first element as init and reduces
         ;; the rest through the coll's own coll-reduce.
         (if (nil? (seq coll))
           (f)
           (let [sentinel reduce-no-init-sentinel]
             (impl coll
                   (fn [acc x] (if (identical? acc sentinel) x (f acc x)))
                   sentinel)))
         (internal-reduce f coll))))
     ([f val coll]
      (let [table @CollReduce--coll-reduce
            impl  (or (get table (type coll))
                      (get table :default))]
        (if impl
          (impl coll f val)
          (internal-reduce f val coll)))))

(defn reduce-kv
  "Reduces a map (or any associative source) with f taking
   accumulator, key, and value. Consults IKVReduce; falls back to
   walking the seq."
  [f init coll]
    (let [table @IKVReduce--kv-reduce
          impl  (or (get table (type coll))
                    (get table :default))]
      (if impl
        (impl coll f init)
        (internal-reduce-kv f init coll))))

) ;; end (when (mino-installed? :protocols) ...)

(when (mino-installed? :multimethods)

;; ---------------------------------------------------------------------------
;; Hierarchies: immutable parent/child/ancestor/descendant relationships.
;;
;; A hierarchy is a map with :parents, :ancestors, and :descendants keys.
;; Values are maps from tag -> set of related tags.
;;
;; (make-hierarchy) returns an empty hierarchy.
;; (derive h child parent) adds a relationship (3-arg) or uses global (2-arg).
;; (underive h child parent) removes a relationship.
;; (parents h tag), (ancestors h tag), (descendants h tag) query relationships.
;; (isa? h child parent) checks if child derives from parent.
;; ---------------------------------------------------------------------------

(def ^:private global-hierarchy
  (atom {:parents {} :ancestors {} :descendants {}}))

;; Bumped on every mutation of global-hierarchy; multimethods compare it
;; against their cached version to invalidate stale dispatch entries when
;; derive / underive runs after methods have populated the cache.
(def ^:private hierarchy-version (atom 0))

(defn make-hierarchy
  "Returns an empty hierarchy."
  [] {:parents {} :ancestors {} :descendants {}})

(defn- tc-ancestors [parents-map node]
  (loop [frontier (get parents-map node #{})
         result #{}]
    (if (empty? frontier)
      result
      (let [p (first frontier)
            rst (disj frontier p)]
        (if (contains? result p)
          (recur rst result)
          (recur (into rst (get parents-map p #{}))
                 (conj result p)))))))

(defn- recompute-hierarchy [h]
  (letfn [(add-descendant [m2 node anc]
            (update m2 anc (fn [s] (conj (or s #{}) node))))]
    (let [pm              (:parents h)
          all-nodes       (into (set (keys pm))
                                (apply concat (vals pm)))
          ancestors-map   (reduce (fn [m node]
                                    (let [ancs (tc-ancestors pm node)]
                                      (if (empty? ancs)
                                        m
                                        (assoc m node ancs))))
                                  {} all-nodes)
          descendants-map (reduce
                            (fn [m node]
                              (reduce (fn [m2 anc]
                                        (add-descendant m2 node anc))
                                      m
                                      (get ancestors-map node #{})))
                            {} all-nodes)]
      (assoc h :ancestors ancestors-map
             :descendants descendants-map))))

(defn- valid-tag? [x]
  ;; A derive tag must be a Named (keyword/symbol) or a record/host-type
  ;; value. mino does not require namespacing.
  (or (keyword? x) (symbol? x) (record-type? x)))

(defn- valid-hierarchy? [h]
  (and (map? h)
       (map? (:parents h))
       (map? (:ancestors h))
       (map? (:descendants h))))

(defn derive
  "Establishes a parent/child relationship between child and parent
   in a hierarchy."
  ([tag parent]
   (when-not (valid-tag? tag)
     (throw (ex-info "derive: tag must be a keyword, symbol, or type"
                     {:child tag :parent parent})))
   (when-not (valid-tag? parent)
     (throw (ex-info "derive: parent must be a keyword, symbol, or type"
                     {:child tag :parent parent})))
   (swap! global-hierarchy derive tag parent)
   (swap! hierarchy-version inc)
   nil)
  ([h tag parent]
   (when-not (valid-hierarchy? h)
     (throw (ex-info "derive: invalid hierarchy"
                     {:h h :child tag :parent parent})))
   (when-not (valid-tag? tag)
     (throw (ex-info "derive: tag must be a keyword, symbol, or type"
                     {:child tag :parent parent})))
   (when-not (valid-tag? parent)
     (throw (ex-info "derive: parent must be a keyword, symbol, or type"
                     {:child tag :parent parent})))
   (when (= tag parent)
     (throw (ex-info "Cannot derive tag from itself"
                     {:child tag :parent parent})))
   (when (contains? (get (:ancestors h) parent #{}) tag)
     (throw (ex-info "Cyclic derivation"
                     {:child tag :parent parent})))
   (let [new-parents (update (:parents h) tag
                             (fn [s] (conj (or s #{}) parent)))]
     (recompute-hierarchy (assoc h :parents new-parents)))))

(defn underive
  "Removes a parent/child relationship between child and parent."
  ([tag parent]
   (swap! global-hierarchy underive tag parent)
   (swap! hierarchy-version inc)
   nil)
  ([h tag parent]
   (when-not (valid-hierarchy? h)
     (throw (ex-info "invalid hierarchy" {:h h})))
   (let [cur (get (:parents h) tag #{})]
     (if (contains? cur parent)
       (let [new-set (disj cur parent)
             new-parents (if (empty? new-set)
                           (dissoc (:parents h) tag)
                           (assoc (:parents h) tag new-set))]
         (recompute-hierarchy (assoc h :parents new-parents)))
       h))))

(defn parents
  "Returns the immediate parents of tag in the hierarchy."
  ([tag] (parents @global-hierarchy tag))
  ([h tag]
   (let [p (get (:parents h) tag)]
     (if (and p (not (empty? p))) p nil))))

(defn ancestors
  "Returns all ancestors of tag in the hierarchy."
  ([tag] (ancestors @global-hierarchy tag))
  ([h tag]
   (let [a (get (:ancestors h) tag)]
     (if (and a (not (empty? a))) a nil))))

(defn descendants
  "Returns all descendants of tag in the hierarchy."
  ([tag] (descendants @global-hierarchy tag))
  ([h tag]
   (let [d (get (:descendants h) tag)]
     (if (and d (not (empty? d))) d nil))))

(defn isa?
  "Returns true if child is equal to or derives from parent."
  ([child parent] (isa? @global-hierarchy child parent))
  ([h child parent]
   (or (= child parent)
       (contains? (get (:ancestors h) child #{}) parent)
       (and (vector? child) (vector? parent)
            (= (count child) (count parent))
            (every? identity (map (fn [c p] (isa? h c p)) child parent))))))

;; ---------------------------------------------------------------------------
;; Multimethods: value-dispatched polymorphism.
;; ---------------------------------------------------------------------------

(defn- prefers? [x y prefer-table hierarchy]
  ;; True when x is preferred over y, considering transitive preferences
  ;; through hierarchy parents (matching Clojure's prefers? semantics).
  (or (contains? (get prefer-table x #{}) y)
      (boolean (some (fn [yp] (prefers? x yp prefer-table hierarchy))
                     (or (parents hierarchy y) #{})))
      (boolean (some (fn [xp] (prefers? xp y prefer-table hierarchy))
                     (or (parents hierarchy x) #{})))))

(defn- find-best-method [methods dval default-val prefer-table hierarchy]
  (let [exact (get methods dval)]
    (if exact
      exact
      (let [matches (reduce-kv
                      (fn [acc k v]
                        (if (and (not= k default-val)
                                 (isa? hierarchy dval k))
                          (conj acc [k v])
                          acc))
                      [] methods)]
        (cond
          (= 0 (count matches)) nil
          (= 1 (count matches)) (second (first matches))
          :else
          (let [preferred (filter
                            (fn [[k _]]
                              (every?
                                (fn [[k2 _]]
                                  (or (= k k2)
                                      (prefers? k k2 prefer-table hierarchy)))
                                matches))
                            matches)]
            (if (= 1 (count preferred))
              (second (first preferred))
              (throw (ex-info
                       (str "Multiple methods match dispatch value: " dval
                            ", none preferred")
                       {:dispatch-val dval
                        :matches (mapv first matches)})))))))))

(defn- create-multimethod
  ([dispatch-fn default-val]
   (create-multimethod dispatch-fn default-val global-hierarchy))
  ([dispatch-fn default-val hierarchy-ref]
   (let [method-table   (atom {})
         prefer-table   (atom {})
         dispatch-cache (atom {})
         seen-hierarchy (atom (deref hierarchy-ref))
         mm (fn [& args]
              ;; Stale-cache check by hierarchy identity: derive /
              ;; underive (on the global atom or a custom :hierarchy
              ;; ref) install a fresh hierarchy value, so an identity
              ;; flip means cached isa? matches may no longer hold.
              (let [h (deref hierarchy-ref)]
                (when-not (identical? h @seen-hierarchy)
                  (reset! dispatch-cache {})
                  (reset! seen-hierarchy h))
                (let [dval    (apply dispatch-fn args)
                      cached  (get @dispatch-cache dval)]
                  (if cached
                    (apply cached args)
                    (let [methods @method-table
                          impl    (or (get methods dval)
                                      (find-best-method methods dval default-val
                                                         @prefer-table
                                                         h)
                                      (get methods default-val))]
                      (if impl
                        (do (swap! dispatch-cache assoc dval impl)
                            (apply impl args))
                        (throw (ex-info
                                 (str "No method in multimethod for"
                                      " dispatch value: " (pr-str dval))
                                 {:dispatch-val dval}))))))))]
     (with-meta mm {:type           :multimethod
                    :method-table   method-table
                    :prefer-table   prefer-table
                    :dispatch-cache dispatch-cache
                    :hierarchy-ref  hierarchy-ref
                    :default        default-val}))))

(defn- register-method [mm dispatch-val f]
  (swap! (:method-table (meta mm)) assoc dispatch-val f)
  (reset! (:dispatch-cache (meta mm)) {})
  mm)

(defmacro defmulti
  "Defines a multimethod with the given dispatch function."
  [mm-name & options]
  (let [options  (if (string? (first options)) (rest options) options)
        options  (if (map? (first options)) (rest options) options)
        dispatch-fn-form (first options)
        kw-opts  (apply hash-map (rest options))
        default-val (get kw-opts :default :default)
        hierarchy-form (get kw-opts :hierarchy)]
    (list 'def mm-name
          (if (some? hierarchy-form)
            (list 'create-multimethod dispatch-fn-form default-val
                  hierarchy-form)
            (list 'create-multimethod dispatch-fn-form default-val)))))

(defmacro defmethod
  "Defines a method for a multimethod."
  [multifn dispatch-val & fn-tail]
  (list 'register-method multifn dispatch-val
        (apply list 'fn fn-tail)))

(defn prefer-method
  "Prefers dispatch-val x over y in multimethod mm."
  [multifn dispatch-val-x dispatch-val-y]
  (swap! (:prefer-table (meta multifn))
         update dispatch-val-x (fn [s] (conj (or s #{}) dispatch-val-y)))
  (reset! (:dispatch-cache (meta multifn)) {})
  multifn)

(defn remove-method
  "Removes the method for dispatch-val from multimethod mm."
  [multifn dispatch-val]
  (swap! (:method-table (meta multifn)) dissoc dispatch-val)
  (reset! (:dispatch-cache (meta multifn)) {})
  multifn)

(defn remove-all-methods
  "Removes all methods from multimethod mm."
  [multifn]
  (reset! (:method-table (meta multifn)) {})
  (reset! (:dispatch-cache (meta multifn)) {})
  multifn)

(defn methods
  "Returns the method table of multimethod mm."
  [multifn]
  @(:method-table (meta multifn)))

(defn get-method
  "Returns the method for dispatch-val, or nil."
  [multifn dispatch-val]
  (get @(:method-table (meta multifn)) dispatch-val))

(defn prefers
  "Returns the prefer-table of multimethod mm."
  [multifn]
  @(:prefer-table (meta multifn)))

;; ---------------------------------------------------------------------------
;; Extensible printer: print-method is a multimethod dispatched on (type x)
;; that routes pr / prn output. Users extend it for their own types via
;; (defmethod print-method :my-type [v] ...). The :default method delegates
;; to pr-builtin, which uses the C formatter and handles every built-in.
;;
;; (set-print-method! print-method) installs the late-binding hook in C so
;; pr / prn consult this multimethod. Before this line runs, pr / prn use
;; the permanently-safe C fallback (the Cortex Q5 invariant).
;; ---------------------------------------------------------------------------

(defmulti print-method
  "Multimethod for readable printing. Dispatched on (type x).
  Extend for user types via (defmethod print-method :my-type [v] ...).
  Methods write to stdout as a side effect."
  type)

(defmethod print-method :default [v] (pr-builtin v))

(set-print-method! print-method)

) ;; end (when (mino-installed? :multimethods) ...)

(defmacro with-out-str
  "Evaluates body with *out* bound to a fresh growable output buffer,
  and returns the accumulated string."
  [& body]
  `(let [b# (out-buffer)]
     (binding [*out* b#]
       ~@body)
     (out-buffer-str b#)))

(defmacro with-in-str
  "Evaluates body with *in* bound to a string-cursor atom holding
  s. read and read-line consume from the cursor as forms or lines
  are taken; the body's value is returned."
  [s & body]
  `(let [a# (atom ~s)]
     (binding [*in* a#]
       ~@body)))

(defn print-str
  "Returns the print-string of args, space-separated, no trailing newline."
  [& xs]
  (with-out-str (apply print xs)))

(defn prn-str
  "Returns the readable-string of args followed by a newline."
  [& xs]
  (with-out-str (apply prn xs)))

(defn println-str
  "Returns the print-string of args followed by a newline."
  [& xs]
  (with-out-str (apply println xs)))

(defn print-simple
  "Writes the plain text form of o (its str form, bypassing the
   print-method dispatch) to w, an output sink such as the buffer
   *out* is bound to inside with-out-str or a string-collecting
   atom. Returns nil."
  [o w]
  (binding [*out* w]
    (print (str o)))
  nil)

(def char-escape-string
  "Returns escape string for char or nil if none."
  {\newline   "\\n"
   \tab       "\\t"
   \return    "\\r"
   \"         "\\\""
   \\         "\\\\"
   \formfeed  "\\f"
   \backspace "\\b"})

(def char-name-string
  "Returns name string for char or nil if none."
  {\newline   "newline"
   \tab       "tab"
   \space     "space"
   \backspace "backspace"
   \formfeed  "formfeed"
   \return    "return"
   \delete    "delete"})

;; with-open's close call is emitted unqualified (~'close) so it
;; resolves in the consumer's namespace at runtime; the JVM macro
;; avoids auto-qualification the same way with (.close ~name).
(defmacro with-open
  "Binds resources, evaluates body, then closes each resource."
  [bindings & body]
  (if (empty? bindings)
    `(do ~@body)
    (let [name (first bindings)
          init (nth bindings 1)
          rest-bindings (drop 2 bindings)]
      `(let [~name ~init]
         (try
           (with-open ~(into [] rest-bindings) ~@body)
           (finally (~'close ~name)))))))

(defmacro with-temp-dir
  "Binds name to a fresh private temp directory, evaluates body, and
   removes the directory (and its contents) on every exit, including a
   throwing one. An optional prefix string names the directory."
  [bindings & body]
  (let [name   (first bindings)
        prefix (nth bindings 1 nil)]
    `(let [~name (if ~prefix (mkdtemp ~prefix) (mkdtemp))]
       (try
         ~@body
         (finally (rm-rf ~name))))))

(defmacro with-temp-file
  "Binds name to a fresh private temp file, evaluates body, and removes
   the file on every exit, including a throwing one. An optional prefix
   string names the file."
  [bindings & body]
  (let [name   (first bindings)
        prefix (nth bindings 1 nil)]
    `(let [~name (if ~prefix (mkstemp ~prefix) (mkstemp))]
       (try
         ~@body
         (finally (rm-rf ~name))))))

(defmacro with-file-lock
  "Acquires an advisory lock on the lockfile, binds name to the lock
   handle, evaluates body, and releases the lock on every exit, including
   a throwing one. An optional opts map is passed to flock."
  [bindings & body]
  (let [name (first bindings)
        lock (nth bindings 1)
        opts (nth bindings 2 nil)]
    `(let [~name (if ~opts (flock ~lock ~opts) (flock ~lock))]
       (try
         ~@body
         (finally (funlock ~name))))))

;; ---------------------------------------------------------------------------
;; Transducers: composable algorithmic transformations. Gated on
;; :transducers -- when off, the C-level `into` primitive handles
;; (into to from) directly and transducer-aware shapes like
;; (transduce xf f coll) raise a MNS002 "capability not installed"
;; diagnostic. Depends on :protocols (reduce-kv lives there).
;;
;; A transducer is a function (rf -> rf) where rf is a reducing function
;; with three arities: ([] init), ([result] completion), ([result input] step).
;;
;; Use (transduce xf f coll) or (into to xf from) to apply.
;; ---------------------------------------------------------------------------

(when (mino-installed? :transducers)

(defn cat
  "A transducer that concatenates the contents of each input."
  [rf]
    (fn ([] (rf))
        ([result] (rf result))
        ([result input]
         (reduce (fn [r v]
                   (let [ret (rf r v)]
                     (if (reduced? ret) (reduced ret) ret)))
                 result input))))

(defn completing
  "Returns a reducing function with a completion step."
  ([f] (completing f identity))
  ([f cf]
   (fn ([] (f))
       ([result] (cf result))
       ([result input] (f result input)))))

(defn unreduced
  "If x is reduced?, returns (deref x), else returns x."
  [x]
  (if (reduced? x) @x x))

(defn ensure-reduced
  "Wraps x in reduced if it is not already reduced."
  [x]
  (if (reduced? x) x (reduced x)))

;; The pipeline walker fuses at most this many stages (PIPELINE_MAX_STAGES
;; in src/prim/core/sequences.c). A longer comp falls back to the slow
;; closure path rather than overrun the walker's fixed stage array.
(def ^:private fusion-max-stages 8)

(defn- fusion-stages
  "Returns the ordered fusible stage descriptors for xform, or nil.
   A comp of standard stage transducers carries a :mino.fusion/stages
   vector (attached by comp); a lone standard stage transducer carries
   a single :mino.fusion/stage map. Anything else (a reduced-producing,
   completion-stateful, or user xform, or a comp with any untagged
   operand) has neither and is nil, so transduce falls back to the slow
   closure path. A chain longer than the walker's fixed stage limit also
   returns nil and falls back. (ADR 65)"
  [xform]
  (let [m (meta xform)
        stages (when m
                 (or (:mino.fusion/stages m)
                     (when-let [s (:mino.fusion/stage m)] [s])))]
    (when (and stages (<= (count stages) fusion-max-stages))
      stages)))

(defn transduce
  "Reduces coll using the transducer xf applied to the reducing
   function f."
  ([xform f coll]
   (transduce xform f (f) coll))
  ([xform f init coll]
   (let [stages (fusion-stages xform)
         result (when stages (__transduce-fuse stages f init coll))]
     (if (and stages (not (identical? result :mino.fusion/no-fuse)))
       ;; Fast path (ADR 65): the whole xform is a comp of standard
       ;; stage transducers within the walker's stage limit. Apply the
       ;; completion arity exactly as the slow path does. The C prim
       ;; still guards against an unrecognised descriptor by returning
       ;; :mino.fusion/no-fuse; the identical? check falls back safely.
       ;; The reduce step already unwraps its loop-stopping Reduced
       ;; exactly once; a Reduced surviving that unwrap must reach the
       ;; completion and pass through unchanged, so no unreduced here.
       ((xform f) result)
       (let [xrf (xform f)]
         (xrf (reduce xrf init coll)))))))

(def ^:private prim-into into)
(defn into
  "Adds all items from from into to. With a transducer, transforms
   items first."
  ([] [])
  ([to] to)
  ([to from] (prim-into to from))
  ([to xform from] (transduce xform conj to from)))

(defn sequence
  "Coerces coll to a (possibly empty) sequence, if it is not already
   one. Will not force a lazy seq. (sequence nil) yields ().

   With a transducer xf, returns a lazy sequence of applying xf to
   coll, or to the pair-wise step of multiple collections. Parallel
   collections stop at the shortest."
  ([coll]
   (cond
     (nil? coll)  ()
     (seq? coll)  coll
     :else        (or (seq coll) ())))
  ([xform coll]
   (let [acc   (atom [])
         xrf   (xform (fn
                      ([] nil)
                      ([result] result)
                      ([_ input] (swap! acc conj input) nil)))
         ;; Emit the buffered outputs as a direct lazy cons-chain whose
         ;; final tail is produced by `tail` (a thunk). Building the chain
         ;; with cons rather than (concat items (step ...)) keeps
         ;; realization O(1) stack per element: walking a long result no
         ;; longer descends a growing concat spine, so a large transduced
         ;; sequence reduces/counts without deep C recursion.
         emit  (fn emit [items i tail]
                 (if (< i (count items))
                   (cons (nth items i) (lazy-seq (emit items (inc i) tail)))
                   (tail)))]
     ((fn step [s]
        (lazy-seq
          (loop [s s]
            (if-let [s (seq s)]
              (let [ret (xrf nil (first s))]
                (if (reduced? ret)
                  (do (xrf nil)
                      (let [items @acc]
                        (reset! acc [])
                        (emit items 0 (fn [] nil))))
                  (let [items @acc]
                    (if (seq items)
                      (do (reset! acc [])
                          (emit items 0 (fn [] (step (rest s)))))
                      (recur (rest s))))))
              (do (xrf nil)
                  (let [items @acc]
                    (reset! acc [])
                    (emit items 0 (fn [] nil))))))))
      coll)))
  ([xform coll & colls]
   ;; Multi-coll variant: pull one element per collection per step and
   ;; pass them all as inputs to the transducer's reducer. The reducer
   ;; is expected to support (rf acc in1 in2 ...) for this to be
   ;; meaningful; map, filter, etc. implement that arity.
   (let [acc   (atom [])
         xrf   (xform (fn
                      ([] nil)
                      ([result] result)
                      ([_ input] (swap! acc conj input) nil)
                      ([_ input & more]
                       (swap! acc conj
                              (apply vector input more))
                       nil)))
         ;; See the single-coll arity: emit buffered outputs as a direct
         ;; lazy cons-chain so realization stays O(1) stack per element.
         emit  (fn emit [items i tail]
                 (if (< i (count items))
                   (cons (nth items i) (lazy-seq (emit items (inc i) tail)))
                   (tail)))]
     ((fn step [ss]
        (lazy-seq
          (loop [ss ss]
            (let [seqs (map1 seq ss)]
              (if (all-some? seqs)
                (let [firsts (map1 first seqs)
                      ret   (apply xrf nil firsts)]
                  (if (reduced? ret)
                    (do (xrf nil)
                        (let [items @acc]
                          (reset! acc [])
                          (emit items 0 (fn [] nil))))
                    (let [items @acc]
                      (if (seq items)
                        (do (reset! acc [])
                            (emit items 0 (fn [] (step (map1 rest seqs)))))
                        (recur (map1 rest seqs))))))
                (do (xrf nil)
                    (let [items @acc]
                      (reset! acc [])
                      (emit items 0 (fn [] nil)))))))))
      (cons coll colls)))))

(defn halt-when
  "Returns a transducer that halts reduction when pred is satisfied."
  ([pred] (halt-when pred nil))
  ([pred retf]
   (fn [rf]
     (fn ([] (rf))
         ([result]
          (if (and (map? result) (contains? result :__halt-when__))
            (get result :__halt-when__)
            (rf result)))
         ([result input]
          (if (pred input)
            (reduced {:__halt-when__
                      (if retf (retf (rf result) input) input)})
            (rf result input)))))))

(defn eduction
  "Returns a traversal recipe over applying the given transducers to
   coll. The result prints and seqs like a seq, but caches nothing:
   every reduce, seq, or print over it re-runs the transducer stack
   (and any side effects) from the source. Takes any number of
   transducers followed by coll."
  [& args]
  (let [coll (last args)
        xfs  (butlast args)
        xf   (if (= 1 (count xfs))
               (first xfs)
               (apply comp xfs))]
    (rerun-seq (fn [] (sequence xf coll)))))

(defn ->Eduction
  "Factory matching the value (eduction xform coll) returns. mino's
   eduction values re-apply the transducer stack per traversal; the
   factory builds one the same way eduction does."
  [xform coll]
  (eduction xform coll))

) ;; end (when (mino-installed? :transducers) ...)

;; --- Array constructors ---
;;
;; object-array, int-array, long-array, etc. are now C primitives that
;; build a MINO_HOST_ARRAY value -- a fixed-length container distinct
;; from MINO_VECTOR. (vector? (object-array 3)) is false, matching JVM
;; arrays. The element-kind tag drives zero-fill semantics on the
;; primitive variants and printing as `#object[...]`. seq returns the
;; elements; aset is not supported (mino has no in-place mutation
;; outside MINO_ATOM/MINO_VOLATILE).
;;
;; into-array is still in Clojure since it's a thin wrapper.

(defn into-array
  "Converts a collection to an Object array."
  ([aseq]      (to-array aseq))
  ([type aseq] (to-array aseq)))

;; --- Compatibility vars ---

(def ^:dynamic *clojure-version*
  "The version of this runtime, as a map with :major :minor :incremental
   and :qualifier keys. CalVer scheme; decoded from (mino-version), which
   returns MINO_VERSION from src/mino.h -- the single source of truth, so
   this can never drift from the C version banner. Decoded with core
   string ops rather than a regex so the version surface stays available
   when a host installs a capability set that omits the regex engine."
  (let [ver  (mino-version)
        n    (count ver)
        dash (loop [i 0]
               (cond (>= i n)           nil
                     (= \- (nth ver i)) i
                     :else              (recur (inc i))))
        head (if dash (subs ver 0 dash) ver)
        qual (when dash (subs ver (inc dash)))
        nums (loop [i 0, start 0, acc []]
               (cond
                 (>= i (count head))  (conj acc (subs head start i))
                 (= \. (nth head i))  (recur (inc i) (inc i)
                                             (conj acc (subs head start i)))
                 :else                (recur (inc i) start acc)))]
    {:major       (parse-long (nth nums 0))
     :minor       (parse-long (nth nums 1))
     :incremental (parse-long (nth nums 2))
     :qualifier   qual}))
(defn clojure-version
  "Returns the runtime version as a printable string."
  []
  (let [v *clojure-version*]
    (str (:major v) "."
         (format "%02d" (:minor v)) "."
         (format "%02d" (:incremental v))
         (when-let [q (:qualifier v)]
           (str "-" q)))))

;; JVM Clojure AOT-compiler dynvars. mino has no AOT compiler; these
;; are defined so user code that binds them around `load` /
;; `compile` calls doesn't throw, matching the JVM-canon surface.
;; They have no observable effect in mino — purely shape-parity.
(def ^:dynamic *compile-path* nil)
(def ^:dynamic *source-path*  "NO_SOURCE_PATH")
(def ^:dynamic *compile-files* false)
(def ^:dynamic *warn-on-reflection* false)
(def ^:dynamic *unchecked-math* false)

(def ^:dynamic *repl*
  "Bound to true in an interactive read-eval-print context, false in
   script execution. Defaults to false."
  false)

(defmacro assert
  ([x] (list 'when-not x (list 'throw "Assert failed")))
  ([x message] (list 'when-not x (list 'throw message))))

(def ^:dynamic *assert*
  "Controls assertion compilation. When false, `assert` is a no-op.
   Defaults to true."
  true)

(def ^:dynamic *print-length*
  "Maximum number of items printed in a single collection (vector,
   list, map, set, chunk, chunked-cons). nil means no limit (the
   default). The remainder is replaced with `...`. Resolved once per
   top-level pr / prn / print / println / pr-str call; nested
   collections share the same limit."
  nil)

(def ^:dynamic *print-level*
  "Maximum nesting depth printed. A collection found at depth >= this
   limit is replaced with `#`. nil means no limit (the default).
   Resolved once per top-level pr / print call."
  nil)

(def ^:dynamic *print-readably*
  "When true (the default), strings are emitted with their quote
   characters and characters with their escape form so the printed
   output round-trips through the reader. When false, strings and
   characters print their underlying bytes — pr/prn behave like
   print/println. Resolved once per top-level pr / print call."
  true)

(def ^:dynamic *print-meta*
  "When true, every value carrying non-nil metadata is printed with
   its meta map prefixed as `^{...} `. When false (the default), meta
   is silent. Resolved once per top-level pr / print call."
  false)

(def ^:dynamic *print-dup*
  "When true, the printer emits forms a reader can reconstruct
   exactly. mino's built-in record / collection / scalar prints are
   already reader-roundtrip-compatible, so the flag is currently an
   information channel for user-installed print-method implementations
   that branch on dup vs. non-dup output. Default false."
  false)

(def ^:dynamic *print-namespace-maps*
  "When true, a map whose keys are keywords (or symbols) sharing a
   common non-empty namespace is printed as `#:ns{:k1 v1, :k2 v2}`
   instead of `{:ns/k1 v1, :ns/k2 v2}`. Default false."
  false)

(def ^:dynamic *flush-on-newline*
  "When true (the default), the I/O sink behind `*out*` is flushed
   automatically after any write that contains a newline. When false,
   the sink stays buffered so consecutive writes coalesce."
  true)

(def ^:dynamic *math-context*
  "Precision/rounding-mode for bigdec division. nil means exact-or-
   throw (mirrors java.math.BigDecimal.divide without MathContext).
   When set, the value is a map of {:precision N :rounding-mode K}
   where N is a positive integer and K is one of: :half-up (default),
   :down, :up, :floor, :ceiling, :half-down, :half-even, :unnecessary.
   :unnecessary throws when rounding would change the value (mirrors
   JVM's ArithmeticException). Resolved by mino_bigdec_div on each
   call."
  nil)

(def ^:private rounding-symbol->keyword
  ;; JVM RoundingMode enum constants → mino's :keyword surface. Lets
  ;; canonical clojuredocs-shaped examples (which write the mode as a
  ;; bare symbol, e.g. (with-precision 1 :rounding HALF_UP ...)) paste
  ;; through without translation.
  '{UP          :up
    DOWN        :down
    CEILING     :ceiling
    FLOOR       :floor
    HALF_UP     :half-up
    HALF_DOWN   :half-down
    HALF_EVEN   :half-even
    UNNECESSARY :unnecessary})

(defmacro with-precision
  "Sets *math-context* to {:precision precision :rounding-mode mode}
  around body. The keyword :rounding takes the next form as a
  rounding-mode keyword (e.g. (with-precision 5 :rounding :half-up
  (/ 1M 3M))) or as a JVM RoundingMode enum symbol (e.g. HALF_UP,
  CEILING). Without :rounding, the mode defaults to :half-up."
  [precision & exprs]
  (let [has-rounding? (and (seq exprs) (= :rounding (first exprs)))
        raw-mode      (if has-rounding? (second exprs) :half-up)
        mode          (if (symbol? raw-mode)
                        (or (rounding-symbol->keyword raw-mode)
                            (throw (str "with-precision: unknown rounding mode "
                                        raw-mode
                                        " (expected UP, DOWN, CEILING, FLOOR, "
                                        "HALF_UP, HALF_DOWN, HALF_EVEN, "
                                        "or UNNECESSARY)")))
                        raw-mode)
        actual-body   (if has-rounding? (drop 2 exprs) exprs)]
    `(binding [*math-context* {:precision ~precision
                               :rounding-mode ~mode}]
       ~@actual-body)))

;; --- Pure-Clojure surface ---

;; Identifier predicates.
(defn ident?
  "Returns true if x is a symbol or keyword."
  [x] (or (symbol? x) (keyword? x)))

(defn simple-ident?
  "Returns true if x is a non-namespace-qualified symbol or keyword."
  [x] (and (ident? x) (nil? (namespace x))))

(defn qualified-ident?
  "Returns true if x is a namespace-qualified symbol or keyword."
  [x] (and (ident? x) (some? (namespace x))))

(def ^:private special-symbols-set
  '#{& . case* catch def deftype* do finally fn fn* if let let* letfn*
     loop loop* new ns quote recur set! throw try var
     binding lazy-seq})

(defn special-symbol?
  "Returns true if x is a symbol that names a special form."
  [s] (contains? special-symbols-set s))

(defn map-entry?
  "Returns true if x is a map entry (mino represents entries as
   2-vectors)."
  [x] (and (vector? x) (= 2 (count x))))

;; Uses eval of a full ns form rather than a direct refer call because mino's
;; :refer-clojure handling (in the C ns special form) additionally detaches the
;; parent env (clojure.core) before rebuilding the filtered mapping, so excluded
;; names are truly hidden rather than falling through the parent chain.
;; The standalone (refer 'clojure.core ...) primitive does not perform this
;; parent-chain detach, so routing through the ns form is the only correct path.
(defmacro refer-clojure
  "Refers public vars from clojure.core into the current namespace,
   accepting the same filter options as the ns :refer-clojure clause:
   :exclude, :only, :rename.  Exclusions and :only limits are honored
   by re-applying the ns form on the current namespace, which cuts the
   clojure.core parent chain and rebuilds the filtered mapping — the
   same path the (ns ...) special form takes."
  [& filters]
  (let [clause (list* :refer-clojure filters)]
    `(eval (list 'ns *ns* '~clause))))

;; bytes? / bitstring? predicates are installed as C primitives -- the
;; real checks against MINO_BYTES live in src/prim/core/reflection.c so they
;; integrate with the type-dispatch fast path. `inst?` does the real
;; check against the `:mino/instant` meta marker that clojure.instant
;; attaches to its parsed maps. mino has no URI type, so uri? stays
;; false.
(defn inst?  [x]
  (boolean (and (map? x) (:mino/instant (meta x)))))
(defn uri?   [x] false)

;; ---------------------------------------------------------------------------
;; Bit-syntax destructure macro.
;;
;; let-bits binds a sequence of segments from a bytes value at running
;; bit offsets. Each segment is a vector starting with a symbol and
;; followed by keyword/value option pairs matching the bits-get
;; surface (:size, :type, :endian, :signed?). The final segment with
;; :type :bytes and no explicit :size binds the bit-aligned remainder.
;;
;; Example:
;;
;;   (let-bits [packet
;;              [a :size 16]
;;              [b :size 32 :endian :little]
;;              [tail :type :bytes]]
;;     (println a b (count tail)))
;;
;; The macro expands into a (let ...) form whose body is the macro
;; arguments after the binding vector.
;; ---------------------------------------------------------------------------

(defn- bits-let-seg-size
  "Compute the static bit count of one segment from its option map.
   Returns :rest when the segment has :type :bytes without :size,
   otherwise an integer. Throws on a malformed segment."
  [opts-map]
  (let [type (or (:type opts-map) :int)
        explicit-size (:size opts-map)]
    (cond
      explicit-size explicit-size
      (= type :float) 64
      (= type :bytes) :rest
      :else 8)))

(defmacro let-bits
  "Destructure-shaped binding over a bytes value.
   (let-bits [bytes-val [sym & opts] ...] body...)
   See documentation in core.clj just above this definition."
  [bindings & body]
  (when-not (vector? bindings)
    (throw (ex-info "let-bits: first form must be a binding vector"
                    {:got bindings})))
  (let [packet-form (first bindings)
        segments    (rest bindings)
        packet-sym  (gensym "packet__")
        total-sym   (gensym "totalbits__")]
    (loop [segs   segments
           offset 0
           pairs  []]
      (if (empty? segs)
        `(let [~packet-sym ~packet-form
               ~total-sym  (* 8 (count ~packet-sym))
               ~@(mapcat identity pairs)]
           ~@body)
        (let [seg (first segs)]
          (when-not (vector? seg)
            (throw (ex-info "let-bits: each segment must be a vector"
                            {:got seg})))
          (let [sym         (first seg)
                opts        (rest seg)
                opts-map    (apply hash-map opts)
                seg-size    (bits-let-seg-size opts-map)
                type        (or (:type opts-map) :int)
                endian      (:endian opts-map)
                signed?     (:signed? opts-map)
                size-form   (if (= seg-size :rest)
                              `(- ~total-sym ~offset)
                              seg-size)
                next-offset (if (= seg-size :rest)
                              total-sym
                              (+ offset seg-size))
                bg          `(bits-get ~packet-sym
                                       :offset ~offset
                                       :size   ~size-form
                                       :type   ~type
                                       ~@(when endian [:endian endian])
                                       ~@(when (some? signed?) [:signed? signed?]))]
            (recur (rest segs)
                   next-offset
                   (conj pairs [sym bg]))))))))

(def ^:private uuid-hex-pattern #"[0-9a-fA-F]+")

(defn- uuid-string?
  "Validates RFC 4122 textual layout: 36 chars with dashes at the
   8/13/18/23 positions and hex digits everywhere else. Avoids the
   {n} quantifier mino's regex engine does not support."
  [s]
  (and (string? s)
       (= 36 (count s))
       (= \- (nth s 8))
       (= \- (nth s 13))
       (= \- (nth s 18))
       (= \- (nth s 23))
       (some? (re-matches uuid-hex-pattern
                          (str (subs s 0  8)
                               (subs s 9  13)
                               (subs s 14 18)
                               (subs s 19 23)
                               (subs s 24 36))))))

;; uuid? is a C primitive (recognises MINO_UUID values).


(defn tagged-literal?
  "Returns true if x is a tagged-literal record produced by
   tagged-literal."
  [value] (boolean (some-> value meta :mino/tagged-literal)))

(defn reader-conditional?
  "Returns true if x is a reader-conditional record produced by
   reader-conditional."
  [value] (boolean (some-> value meta :mino/reader-conditional)))

;; find-keyword is a C primitive: a lookup-only probe over the
;; keyword intern table (nil when absent, never interns).

;; Parsing helpers (Clojure 1.11+).
(defn parse-boolean
  "Parses 'true' or 'false' (case-sensitive) and returns the boolean.
   Returns nil for strings that don't match. Per Clojure's contract
   raises an error on non-string input (analogous to JVM's
   ClassCastException / NullPointerException)."
  [s]
  (when-not (string? s)
    (throw (ex-info "parse-boolean: argument must be a string"
                    {:value s :type (type s)})))
  (case s "true" true "false" false nil))

;; parse-uuid is a C primitive (returns a MINO_UUID value or nil).


;; Eager-vector partitioning (Clojure 1.13+ surface, useful pre-1.13).
(defn partitionv
  "Like partition but returns a lazy seq of vectors instead of lists."
  ([n coll]          (map vec (partition n coll)))
  ([n step coll]     (map vec (partition n step coll)))
  ([n step pad coll] (map vec (partition n step pad coll))))

(defn partitionv-all
  "Like partition-all but returns a lazy seq of vectors instead of
   lists."
  ([n coll]      (map vec (partition-all n coll)))
  ([n step coll] (map vec (partition-all n step coll))))

(defn splitv-at
  "Returns a vector [(vec (take n coll)) (drop n coll)]: the head is a
   vector, the tail stays a lazy seq."
  [n coll] [(vec (take n coll)) (drop n coll)])

(defn replicate
  "Returns a lazy seq of n copies of x. Deprecated alias for
   (take n (repeat x))."
  [n x] (take n (repeat x)))

(defn list*
  "Creates a new list containing the items prepended to the rest, the
   last of which will be treated as a sequence."
  ([args] (seq args))
  ([a args] (cons a args))
  ([a b args] (cons a (cons b args)))
  ([a b c args] (cons a (cons b (cons c args))))
  ([a b c d & more] (cons a (cons b (cons c (cons d (apply list* more)))))))

(defn reset-meta!
  "Atomically resets the metadata for a reference type to meta-map.
   Returns meta-map."
  [iref metadata-map]
  (alter-meta! iref (constantly metadata-map))
  metadata-map)

;; Collection-hash helpers. Real Clojure mixes via Murmur3; mino uses
;; a simpler combiner that is consistent across runs but does not
;; match Clojure's exact bit pattern. Suitable for in-process equality
;; bookkeeping; not for cross-runtime hash compatibility.
(defn mix-collection-hash
  "Combines a hash-basis with the collection's count."
  [hash-basis count]
  (bit-xor (or hash-basis 0) (or count 0)))

(defn hash-combine
  "Boost-style hash combiner: mixes seed and hash into a single 32-bit
  hash. Matches clojure.core/hash-combine bit-for-bit so user code that
  manually composes hashes via this helper sees the same result on
  mino as on JVM Clojure.

    seed ^= hash + 0x9e3779b9 + (seed << 6) + (seed >> 2)

  The operation is performed in unchecked 32-bit arithmetic; the result
  is truncated to the low 32 bits."
  [x y]
  (let [x (or x 0)
        y (or y 0)]
    (unchecked-int
      (bit-xor x
               (unchecked-add y
                              (unchecked-add 0x9e3779b9
                                             (unchecked-add
                                               (bit-shift-left x 6)
                                               (bit-shift-right x 2))))))))

(defn hash-ordered-coll
  "Computes a sequence-position-aware hash for an ordered collection."
  [coll]
  (mix-collection-hash
    (reduce (fn [h v] (bit-xor (* 31 h) (hash v))) 1 coll)
    (count coll)))

(defn hash-unordered-coll
  "Computes a position-independent hash for an unordered collection."
  [coll]
  (mix-collection-hash
    (reduce + 0 (map hash coll))
    (count coll)))

;; Exception cause (mino has no Throwable.getCause, so this reads from
;; ex-data or attached metadata).
(defn ex-cause
  "Returns the cause attached to the given exception, or nil."
  [ex]
  (or (some-> ex ex-data :cause)
      (some-> ex meta :cause)))

(defn Throwable->map
  "Constructs a data representation of an error value, shaped
   {:cause m :data d :via [...] :trace []}: :cause is the root
   cause's message, :data its ex-data (absent when nil), :via a
   vector of {:type <symbol> :message <str> :at [] :data <optional>}
   maps from the outermost error to the root, and :trace an empty
   vector (mino error values do not retain call-stack frames).
   Works on caught diagnostic maps and on ex-info values alike;
   the cause chain is walked via ex-cause."
  [o]
  (let [chain (loop [e o acc []]
                (let [acc (conj acc e)
                      c   (ex-cause e)]
                  (if (map? c) (recur c acc) acc)))
        root  (peek chain)
        via   (mapv (fn [e]
                      (let [d (ex-data e)
                            typ (if (some? d)
                                  'clojure.lang.ExceptionInfo
                                  'java.lang.Exception)
                            m {:type typ :message (ex-message e) :at []}]
                        (if (some? d) (assoc m :data d) m)))
                    chain)
        base  {:cause (ex-message root)
               :via   via
               :trace []}
        d     (ex-data root)]
    (if (some? d) (assoc base :data d) base)))

;; inst-ms: returns epoch millis from an inst (a map with the
;; `:mino/instant true` meta marker, as produced by
;; clojure.instant/read-instant-date or the #inst reader literal).
;; The conversion is duplicated here from clojure.instant so it works
;; without an explicit (require 'clojure.instant); the two impls
;; share the same component-map contract.
(let [days-before-month [0 0 31 59 90 120 151 181 212 243 273 304 334]]
  (defn- inst-ms-leap? [y]
    (or (and (zero? (mod y 4)) (not (zero? (mod y 100))))
        (zero? (mod y 400))))
  (defn- inst-ms-count-leaps [y]
    (- (+ (quot y 4) (quot y 400)) (quot y 100)))
  (defn- inst-ms-days-from-1970 [y m d]
    (let [year-days    (* 365 (- y 1970))
          leap-correct (- (inst-ms-count-leaps (dec y))
                          (inst-ms-count-leaps 1969))
          month-days   (nth days-before-month m)
          leap-of-y    (if (and (> m 2) (inst-ms-leap? y)) 1 0)]
      (+ year-days leap-correct month-days leap-of-y (dec d)))))

(defn inst-ms
  "Returns epoch millis (since 1970-01-01T00:00:00Z) for an inst
   value as returned by clojure.instant/read-instant-date or the
   `#inst \"...\"` reader literal. Throws on a non-inst argument."
  [inst]
  (when-not (inst? inst)
    (throw (ex-info "inst-ms: not an inst" {:got inst})))
  (let [{:keys [years months days hours minutes seconds nanoseconds
                offset-sign offset-hours offset-minutes]} inst
        epoch-days (inst-ms-days-from-1970 years months days)
        local-ms   (+ (* 1000 (+ (* epoch-days 86400)
                                  (* hours 3600)
                                  (* minutes 60)
                                  seconds))
                      (quot nanoseconds 1000000))
        offset-ms  (* (or offset-sign 1)
                      (+ (* (or offset-hours 0)   3600000)
                         (* (or offset-minutes 0) 60000)))]
    ;; offset is east-of-UTC; subtract to reach UTC millis.
    (- local-ms offset-ms)))

;; Inst protocol. mino insts are maps carrying the :mino/instant meta
;; marker, and protocol dispatch keys on (type x), so the extension
;; registers under :map and inst-ms validates the marker. Divergence:
;; (satisfies? Inst m) is true for any map, not only insts.
(when (mino-installed? :protocols)

(defprotocol Inst
  (inst-ms* [inst]))

(extend-type :map Inst
  (inst-ms* [v] (inst-ms v)))

) ;; end (when (mino-installed? :protocols) ...)

;; Reader literals. The #inst handler lazily requires clojure.instant
;; the first time #inst is encountered so core boot doesn't load the
;; bigger ISO 8601 parser unless needed. #uuid is handled directly by
;; the reader (src/eval/read.c) before *data-readers* is consulted;
;; its entry here covers callers that look the reader fn up by tag.
(let [inst-reader (fn inst-reader [s]
                    (require 'clojure.instant)
                    ((resolve 'clojure.instant/read-instant-date) s))
      uuid-reader (fn uuid-reader [s]
                    (or (parse-uuid s)
                        (throw (ex-info (str "Invalid UUID string: "
                                             (pr-str s))
                                        {:input s}))))]
  (def default-data-readers
    "Default map of data reader functions keyed by tag symbol: 'inst
     and 'uuid."
    {'inst inst-reader
     'uuid uuid-reader})
  (alter-var-root #'*data-readers* assoc 'inst inst-reader 'uuid uuid-reader))

;; read: Clojure-compatible reader entry point.
;;   (read)        — reads one form from *in*. Atom-bound *in*
;;                   consumes from the head; the default stdin
;;                   sentinel raises an unsupported error (use
;;                   with-in-str or read-string instead).
;;   (read s)      — reads from the given string.
;;   (read opts s) — reads from the string with the given options.
(defn read
  ([]       (read*))
  ([s]      (read-string s))
  ([opts s] (read-string opts s)))

(defn- read+string-trim
  "Strips leading and trailing ASCII whitespace from s. Local helper
   for read+string; clojure.string is not loaded during core boot."
  [s]
  (let [ws?   (fn [c] (or (= c \space) (= c \tab) (= c \newline)
                          (= c \return) (= c \formfeed)))
        n     (count s)
        start (loop [i 0]
                (if (and (< i n) (ws? (nth s i))) (recur (inc i)) i))
        end   (loop [i n]
                (if (and (> i start) (ws? (nth s (dec i))))
                  (recur (dec i))
                  i))]
    (subs s start end)))

(defn read+string
  "Like read: consumes one form from the source and returns
   [form text] where text is the exactly-consumed input, whitespace-
   trimmed. The source may be a string or a string-cursor atom of the
   kind *in* / with-in-str use (the cursor advances past the form)."
  [s]
  (let [a      (if (string? s) (atom s) s)
        before (deref a)
        form   (binding [*in* a] (read*))
        after  (deref a)
        taken  (subs before 0 (- (count before) (count after)))]
    [form (read+string-trim taken)]))

(defn line-seq
  "Returns the lines of text from rdr as a lazy sequence of strings.
   rdr is a string-cursor atom (the *in* model that read-line
   consumes from): each realized element takes one line off the
   cursor. Returns nil when the cursor is exhausted. Throws for a
   rdr that is not an atom; per JVM line-seq, a value that is not a
   reader cannot be consumed."
  [rdr]
  (when-not (atom? rdr)
    (throw (ex-info "line-seq: argument must be a string-cursor atom"
                    {:value rdr})))
  (when-let [line (binding [*in* rdr] (read-line))]
    (cons line (lazy-seq (line-seq rdr)))))

;; Reader placeholders. Syntax-quote handles ~ and ~@ itself, so
;; these exist only so the names resolve; calling either one means
;; the form escaped syntax-quote, which is an error.
(defn unquote
  "Placeholder for the ~ reader form. Only meaningful inside
   syntax-quote; calling it directly throws."
  [& _]
  (throw (ex-info "unquote (~) is only valid inside syntax-quote"
                  {:mino/unsupported :unquote})))

(defn unquote-splicing
  "Placeholder for the ~@ reader form. Only meaningful inside
   syntax-quote; calling it directly throws."
  [& _]
  (throw (ex-info "unquote-splicing (~@) is only valid inside syntax-quote"
                  {:mino/unsupported :unquote-splicing})))

(defn test
  "Finds fn at key :test in v's metadata, calls it (presumably with
   no side effects on the wider system), and reports :ok when it
   returns, :no-test when no :test fn is present. An error thrown by
   the :test fn flows out unwrapped."
  [v]
  (let [f (:test (meta v))]
    (if f
      (do (f) :ok)
      :no-test)))

;; bound? / thread-bound?: variadic over vars, true iff every var has a
;; binding of the right shape. The C primitives -var-root-bound? and
;; -thread-bound? do the per-var check.
(defn bound?
  "Returns true if all of the vars provided as arguments have any
   bindings — either a root binding or a thread-local binding."
  [& vars]
  (every? (fn [v] (or (-var-root-bound? v) (-thread-bound? v))) vars))

(defn thread-bound?
  "Returns true if all of the vars provided as arguments have
   thread-local bindings active on the current dyn-stack."
  [& vars]
  (every? -thread-bound? vars))

;; with-bindings / push/pop-thread-bindings: wrap the C primitives so the
;; Clojure-level surface matches clojure.lang.Var/pushThreadBindings.
(defn push-thread-bindings
  "Push a fresh dynamic-binding frame whose entries come from the map.
   Symbols-or-strings are accepted as keys. Must be paired with
   pop-thread-bindings in a try/finally."
  [bindings]
  (push-thread-bindings* bindings))

(defn pop-thread-bindings
  "Pop the topmost dynamic-binding frame. Throws when no frame is
   active. Pair with push-thread-bindings."
  []
  (pop-thread-bindings*))

(defmacro with-bindings
  "Takes a map of var->value pairs. Installs the bindings, executes
   body, and pops the bindings in a finally clause."
  [binding-map & body]
  `(let [vmap# ~binding-map]
     (push-thread-bindings vmap#)
     (try
       ~@body
       (finally
         (pop-thread-bindings)))))

;; bound-fn / bound-fn*: capture the dynamic-binding context active at
;; capture time and replay it around every invocation of the wrapped fn.
;; Layered on the C primitives get-thread-bindings + with-bindings*.
(defn bound-fn*
  "Returns a function which installs the same bindings in effect as in
   the thread at the time bound-fn* was called and then invokes f."
  [f]
  (let [bindings (get-thread-bindings)]
    (fn [& args]
      (with-bindings* bindings (fn [] (apply f args))))))

(defmacro bound-fn
  "Returns a function defined by the given fntail, which will install
   the same bindings in effect as in the thread at the time bound-fn
   was called."
  [& fntail]
  `(bound-fn* (fn ~@fntail)))

;; with-redefs-fn: low-level redef (the macro variant lives above).
(defn with-redefs-fn
  "Temporarily rebinds the root values of vars to new-values while
   thunk runs, restoring originals afterward. bindings-map is a map
   of var -> new-value."
  [binding-map func]
  (let [pairs (vec binding-map)
        olds  (mapv (fn [pair] [(first pair) (-var-root (first pair))]) pairs)]
    (try
      (doseq [pair pairs]
        (alter-var-root (first pair) (constantly (second pair))))
      (func)
      (finally
        (doseq [pair olds]
          (alter-var-root (first pair) (constantly (second pair))))))))

;; Tap mechanism: a registry of fns invoked from tap>.
(def ^:private tap-fns (atom #{}))

(defn add-tap
  "Registers f as a tap target. Each call to tap> invokes every
   registered tap with the tapped value. Returns nil."
  [f]
  (swap! tap-fns conj f)
  nil)

(defn remove-tap
  "Unregisters f from the tap registry. Returns nil."
  [f]
  (swap! tap-fns disj f)
  nil)

(defn tap>
  "Sends x to every registered tap. Tap fns that throw are silently
   skipped so a misbehaving subscriber does not poison the stream.
   Returns true."
  [x]
  (doseq [f @tap-fns]
    (try (f x) (catch __e nil)))
  true)

;; Reader-extension records. tagged-literal and reader-conditional are
;; exposed as constructors so portable Clojure code can build round-
;; trippable values without depending on host-specific record types.
(defn tagged-literal
  "Builds a tagged-literal record with tag and form fields. Predicate
   tagged-literal? returns true on the result."
  [tag form]
  (with-meta {:tag tag :form form} {:mino/tagged-literal true}))

(defn reader-conditional
  "Builds a reader-conditional record with form and splicing? fields.
   Predicate reader-conditional? returns true on the result."
  [form splicing?]
  (with-meta {:form form :splicing? (boolean splicing?)}
             {:mino/reader-conditional true}))

;; clojure.lang.PersistentQueue/EMPTY — the canonical empty persistent
;; queue. Clojure programs use this with (conj ...) to build queues.
;; mino has no Java-class machinery; we expose the canonical name as a
;; var in the clojure.lang.PersistentQueue namespace so the slash-form
;; lookup works without special-casing.
(in-ns 'clojure.lang.PersistentQueue)
(def EMPTY (-empty-queue))
(in-ns 'clojure.core)

;; --- Platform-specific forms (not supported on mino) ---
;;
;; mino is neither a JVM nor a JavaScript runtime, so forms that
;; generate Java classes (defrecord, deftype, reify, proxy,
;; gen-class, definterface), import Java packages (import), or
;; perform Java instance-of checks (instance?) cannot be honored.
;; Each one throws an informative ex-info at expansion or call time
;; rather than silently no-op'ing or returning a fake value, so users
;; learn about the gap immediately and library failures point at the
;; real cause.

(defn- defrecord-bind-fields-in-method
  "Wraps a protocol method body so the record's field names are
   visible as locals bound to (get this :field). Matches Clojure
   defrecord's contract: in (defrecord R [a b] IFoo (bar [this] a)),
   `a` resolves to (:a this) inside the method body.

   Returns the method form unchanged if it doesn't look like a
   method spec (`(symbol [vec] body...)`), so protocol-name
   separators inside specs pass through. Bypasses wrapping when
   there are no fields. Skips binding for fields whose name shadows
   the method's first param (i.e. the dispatching `this` slot).

   Order of bindings inside the let preserves the field declaration
   order so later fields can reference earlier ones if a user shadows
   intentionally."
  [fields method]
  (letfn [(bind-clause [params body]
            (let [this-sym (first params)
                  binds    (vec (mapcat
                                  (fn [f]
                                    (if (= f this-sym)
                                      []
                                      [f (list 'get this-sym
                                               (keyword (str f)))]))
                                  fields))]
              (if (seq binds)
                (list params (apply list 'let binds body))
                (apply list params body))))]
    (if (and (or (list? method) (cons? method))
             (seq method)
             (symbol? (first method))
             (seq fields))
      (let [mname (first method)
            tail  (rest method)]
        (cond
          ;; (m [params] body...)
          (vector? (first tail))
          (cons mname (bind-clause (first tail) (rest tail)))
          ;; (m ([p] body...) ([p k] body...))
          (and (seq tail)
               (every? (fn [c] (and (seq? c) (vector? (first c)))) tail))
          (apply list mname
                 (map (fn [c] (bind-clause (first c) (rest c))) tail))
          :else method))
      method)))

(defmacro defrecord
  "Defines a record type Name with the given fields and optional
   inline protocol specs. Establishes:
     Name       — the MINO_TYPE value (used by extend-type and
                  instance? as the dispatch key)
     ->Name     — positional constructor: (->Name f1 f2 ...) returns
                  a record value
     map->Name  — map constructor: (map->Name {:f1 v1 :f2 v2}) reads
                  declared fields from the map; non-field keys land
                  in ext.

   Fields must be a vector of symbols; they are stored as keywords
   on the type. Specs follow the same shape as extend-type:
   protocol-name followed by one or more (method [args] body) forms.

   Inside an inline protocol method body, field names resolve as
   locals bound to (get this :field) -- matches Clojure's defrecord
   contract so (defrecord R [a b] IFoo (bar [this] (+ a b))) works
   without writing (:a this) / (:b this) by hand."
  [name fields & specs]
  (when-not (vector? fields)
    (throw (str "defrecord: fields must be a vector, got: "
                (pr-str fields))))
  (let [ns-str     (str (ns-name *ns*))
        name-str   (str name)
        ctor       (symbol (str "->" name))
        map-ctor   (symbol (str "map->" name))
        field-kws  (mapv (fn [f] (keyword (str f))) fields)
        bind-meth  (fn [m] (defrecord-bind-fields-in-method fields m))
        wrap-spec  (fn [s]
                     (if (and (or (list? s) (cons? s))
                              (seq s)
                              (symbol? (first s)))
                       (bind-meth s)
                       s))
        specs*     (mapv wrap-spec specs)
        forms      [(list 'def name (list 'defrecord* ns-str name-str field-kws))
                    (list 'defn ctor fields
                          (list 'record* name (vec fields)))
                    (list 'defn map-ctor ['m]
                          (list 'record-from-map name 'm))]]
    (apply list 'do (if (seq specs*)
                      (conj forms (apply list 'extend-type name specs*))
                      forms))))

(defmacro deftype
  "Alias for defrecord. mino has no separate JVM-class layer to
   expose, so the deftype/defrecord distinction collapses; values
   created either way are real types with map-isomorphic behaviour."
  [name fields & specs]
  (when-not (vector? fields)
    (throw (str "deftype: fields must be a vector, got: "
                (pr-str fields))))
  (apply list 'defrecord name fields specs))

(defmacro reify
  "Returns an instance of a fresh anonymous record type that
   satisfies the named protocols. Each reify form generates one
   type at expansion time; repeated invocations of the form share
   that type, so (= (type r1) (type r2)) is true for two values
   produced by the same reify form."
  [& opts+specs]
  (let [ns-str   (str (ns-name *ns*))
        sym      (gensym "reify_T_")
        name-str (str sym)
        T        (gensym "T")]
    (list 'let [T (list 'defrecord* ns-str name-str [])]
          (apply list 'extend-type T opts+specs)
          (list 'record* T []))))

(def ^:private munge-char-map
  "The compiler CHAR_MAP: each character that is illegal in a host
   identifier maps to a reserved token. Alphanumerics and '.' are
   absent, so they pass through munge unchanged."
  {\-  "_",             \:  "_COLON_",       \+  "_PLUS_",
   \>  "_GT_",          \<  "_LT_",          \=  "_EQ_",
   \~  "_TILDE_",       \!  "_BANG_",        \@  "_CIRCA_",
   \#  "_SHARP_",       \'  "_SINGLEQUOTE_", \"  "_DOUBLEQUOTE_",
   \%  "_PERCENT_",     \^  "_CARET_",       \&  "_AMPERSAND_",
   \*  "_STAR_",        \|  "_BAR_",         \{  "_LBRACE_",
   \}  "_RBRACE_",      \[  "_LBRACK_",      \]  "_RBRACK_",
   \/  "_SLASH_",       \\  "_BSLASH_",      \?  "_QMARK_"})

(defn munge
  "Encodes s into a string legal as a host identifier, replacing every
   character in the compiler CHAR_MAP with its reserved token. Symbols
   munge to symbols; anything else munges to a string."
  [s]
  ((if (symbol? s) symbol str)
   (apply str (map (fn [c] (get munge-char-map c c)) (str s)))))

(defmacro definline
  "Experimental - like defmacro, except defines a named function whose
   body is the expansion of the template expr. On mino the :inline hint
   is advisory (the JIT, not a source inliner, supplies the speed the
   hint asks for on other hosts); the defined var is an ordinary,
   first-class function. Cannot be used with variadic (&) args."
  [name & decl]
  (let [[pre-args [args expr]] (split-with (comp not vector?) decl)]
    `(do
       (defn ~name ~@pre-args ~args ~(apply (eval (list `fn args expr)) args))
       (alter-meta! (var ~name) assoc :inline (fn ~name ~args ~expr))
       (var ~name))))

(defmacro proxy [& _]
  (throw (ex-info
           "proxy is not supported on mino — there is no JVM to subclass"
           {:mino/unsupported :proxy})))

(defmacro gen-class [& options]
  (throw (ex-info
           (str "gen-class is not supported on mino — there is no"
                " JVM to compile against")
           {:mino/unsupported :gen-class})))

(defmacro definterface [& _]
  (throw (ex-info
           (str "definterface is not supported on mino — use"
                " defprotocol instead")
           {:mino/unsupported :definterface})))

(defmacro import [& import-symbols-or-lists]
  (throw (ex-info
           (str "Java import is not supported on mino — there are no"
                " Java classes to import")
           {:mino/unsupported :import})))

(defn instance?
  "Returns true if x is an instance of c. For record types defined
   with defrecord, c is the type value and the test is type-pointer
   identity. For built-in types or ad-hoc :type-tagged values, c may
   be the keyword (type x) returns and the test is keyword equality.
   A small set of frozen capability keywords (ADR 72) answer by
   capability instead: :editable-collection is true for the
   collections that support transients."
  [c x]
  (if (identical? c :editable-collection)
    (contains? #{:map :vector :set} (type x))
    (= c (type x))))

;; In Clojure JVM the primed arithmetic forms (`+'`, `-'`, `*'`,
;; `inc'`, `dec'`) auto-promote to BigInt; the unprimed forms throw
;; on overflow. mino now matches that contract: the unprimed +/-/*/
;; inc/dec primitives throw on long overflow, and the primed forms
;; have their own C primitives that auto-promote. No aliases needed
;; -- both arms are first-class.

(defmacro with-redefs
  "Temporarily rebinds the root bindings of vars while body executes, restoring
   them in a finally clause. Bindings is a vector of var-name/value pairs.

   The temp-value exprs are evaluated in parallel BEFORE any rebind fires, so a
   later binding-value that names an earlier-listed var sees that var's
   pre-redef value (matching Clojure JVM)."
  [bindings & body]
  (let [pairs    (partition 2 bindings)
        var-syms (map first pairs)
        new-vals (map second pairs)
        olds     (map (fn [_] (gensym "old")) pairs)
        news     (map (fn [_] (gensym "new")) pairs)
        sets     (map (fn [v new-sym]
                        (list 'alter-var-root (list 'var v)
                              (list 'fn ['_] new-sym)))
                      var-syms news)
        restores (map (fn [v old]
                        (list 'alter-var-root (list 'var v)
                              (list 'fn ['_] old)))
                      var-syms olds)
        finally-form (apply list 'finally restores)
        try-form     (apply list 'try (concat sets body (list finally-form)))]
    (list 'let
          (vec (concat
                 (mapcat (fn [old v] [old (list '-var-root (list 'var v))])
                         olds var-syms)
                 (mapcat (fn [new-sym new-val] [new-sym new-val])
                         news new-vals)))
          try-form)))

(defmacro with-local-vars
  "Binds names to fresh, lexically-scoped vars holding init values.
   Within body the names refer to vars: read with @name, mutate with
   (var-set name val). The vars are interned in the current namespace
   under gensym'd suffixes so they don't collide with named defs."
  [name-vals-vec & body]
  (let [pairs (partition 2 name-vals-vec)
        let-pairs (mapcat (fn [pair]
                            (let [n    (first pair)
                                  init (first (rest pair))]
                              [n (list 'intern '*ns*
                                       (list 'gensym (str (name n) "__lv__"))
                                       init)]))
                          pairs)]
    (apply list 'let (vec let-pairs) body)))

