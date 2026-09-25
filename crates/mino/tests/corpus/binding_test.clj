(require "tests/test")

;; Bindings: def, let, lexical scope, redefinition.

(deftest def-then-read
  (def x__bt 41)
  (is (= 42 (+ x__bt 1))))

(deftest def-redefine
  (def r__bt 1)
  (is (= 1 r__bt))
  (def r__bt 2)
  (is (= 2 r__bt)))

(deftest let-binding
  (testing "single"
    (is (= 5 (let [x 5] x))))
  (testing "multi"
    (is (= 3 (let [x 1 y 2] (+ x y)))))
  (testing "sequential"
    (is (= 11 (let [x 1 y (+ x 10)] y))))
  (testing "shadow"
    (def a__bt 1)
    (is (= 99 (let [a__bt 99] a__bt)))
    (is (= 1 a__bt))))

(deftest var-redef-closure
  (def vrc-f (fn [] vrc-x))
  (def vrc-x 10)
  (is (= 10 (vrc-f)))
  (def vrc-x 20)
  (is (= 20 (vrc-f))))

(def ^:dynamic *bt-dyn-x* :outer)
(def bt-plain-x :outer)

(deftest binding-on-dynamic-var-rebinds
  (is (= :inner (binding [*bt-dyn-x* :inner] *bt-dyn-x*)))
  (is (= :outer *bt-dyn-x*)))

(deftest binding-on-non-dynamic-var-throws
  ;; JVM Clojure throws "Can't dynamically bind non-dynamic var" when
  ;; `binding` rebinds a var that wasn't declared ^:dynamic. mino used
  ;; to accept this silently, which masks the difference and lets
  ;; subtle bugs propagate to a real Clojure deployment that then
  ;; fails. Match the canon behaviour: fail loud at the binding site.
  (let [err (try (eval '(binding [bt-plain-x :inner] bt-plain-x)) nil
                 (catch e (if (map? e) (:mino/message e) (str e))))]
    (is (some? err))
    (is (some? (re-find #"dynamic" err)))
    (is (some? (re-find #"bt-plain-x" err)))))

;; Canon parity: bound? / thread-bound? / with-bindings / push+pop-thread-bindings.
;; The C primitives -var-root-bound? + -thread-bound? + with-bindings* +
;; get-thread-bindings + push/pop-thread-bindings* back the Clojure-level wrappers.

(def ^:dynamic *bt-dyn-y* :root)

(deftest bound?-checks-root-or-thread
  (is (bound? (var *bt-dyn-y*)))
  (binding [*bt-dyn-y* :inner]
    (is (bound? (var *bt-dyn-y*))))
  (is (bound? (var *bt-dyn-y*) (var bt-plain-x))))

(deftest thread-bound?-checks-only-thread
  (is (not (thread-bound? (var *bt-dyn-y*))))
  (binding [*bt-dyn-y* :inner]
    (is (thread-bound? (var *bt-dyn-y*))))
  (is (not (thread-bound? (var *bt-dyn-y*)))))

(deftest with-bindings-installs-and-pops
  (is (= :root *bt-dyn-y*))
  (with-bindings {(var *bt-dyn-y*) :wb-inner}
    (is (= :wb-inner *bt-dyn-y*)))
  (is (= :root *bt-dyn-y*)))

(deftest push-pop-thread-bindings-pair
  (is (= :root *bt-dyn-y*))
  (push-thread-bindings {(var *bt-dyn-y*) :pushed})
  (try
    (is (= :pushed *bt-dyn-y*))
    (finally
      (pop-thread-bindings)))
  (is (= :root *bt-dyn-y*)))

(deftest with-bindings-snapshot-via-get-thread-bindings
  ;; get-thread-bindings returns a map keyed by symbols; with-bindings*
  ;; accepts that snapshot directly. Used by bound-fn* to replay
  ;; captured bindings.
  (binding [*bt-dyn-y* :snap]
    (let [snap (get-thread-bindings)]
      (is (map? snap))
      ;; Re-apply outside the binding: with-bindings* takes the snapshot
      ;; and runs the thunk with those names installed.
      (is (= :snap (with-bindings* snap (fn [] *bt-dyn-y*)))))))

(deftest binding-frame-unwinds-on-throw
  ;; A throw inside a binding body must restore the outer value.
  (try
    (binding [*bt-dyn-y* :will-unwind]
      (throw :boom))
    (catch _e nil))
  (is (= :root *bt-dyn-y*)))

;; ---------------------------------------------------------------------------
;; Var-identity binding: rebinding via any spelling of a var's name must reach
;; every read of that var -- bare, namespace-qualified, or alias-qualified.
;; The binding key is the VAR, not the literal symbol text.
;; ---------------------------------------------------------------------------

;; Establish a home namespace with a dynamic var and a reader fn.
(ns dyn.bind.home)
(def ^:dynamic *dbt-v* :root)
(defn dbt-rd [] *dbt-v*)

(in-ns 'user)
(alias 'dbt-h 'dyn.bind.home)

(deftest qualified-binding-visible-to-unqualified-reader
  ;; binding via the qualified name must reach the bare-symbol read
  ;; inside the var's home namespace.
  (is (= :bound (binding [dyn.bind.home/*dbt-v* :bound]
                   (dyn.bind.home/dbt-rd)))))

(deftest alias-binding-visible-to-unqualified-reader
  ;; binding via an alias-qualified name is the same var; the bare read
  ;; in the home fn must see it.
  (is (= :bound2 (binding [dbt-h/*dbt-v* :bound2]
                    (dyn.bind.home/dbt-rd)))))

(deftest qualified-binding-visible-via-qualified-read
  ;; a qualified symbol read must also check the dynamic binding stack,
  ;; not only the var's root.
  (is (= :b3 (binding [dyn.bind.home/*dbt-v* :b3]
                dyn.bind.home/*dbt-v*))))

(deftest nested-qualified-bindings-stack-and-restore
  ;; inner binding shadows outer; both restore correctly.
  (is (= :b5 (binding [dyn.bind.home/*dbt-v* :b4]
                (binding [dyn.bind.home/*dbt-v* :b5]
                  (dyn.bind.home/dbt-rd)))))
  (is (= :b4 (binding [dyn.bind.home/*dbt-v* :b4]
                (binding [dyn.bind.home/*dbt-v* :b5])
                (dyn.bind.home/dbt-rd))))
  (is (= :root (dyn.bind.home/dbt-rd))))

;; Core vars: a bare read in any namespace and a qualified read must both
;; see the thread binding regardless of which spelling was used to install it.

(deftest core-var-qualified-binding-bare-read
  ;; bind via clojure.core/*print-length*; bare *print-length* in user
  ;; resolves to the same var and must see the thread value.
  (is (= 3 (binding [clojure.core/*print-length* 3] *print-length*))))

(deftest core-var-bare-binding-qualified-read
  ;; bind via the bare name; qualified read must see the same thread value.
  (is (= 3 (binding [*print-length* 3] clojure.core/*print-length*))))

;; Frame cleanup: a qualified binding frame must unwind on throw even
;; though the lookup is currently broken, leaving the var root intact.

(ns dyn.bind.throw.home)
(def ^:dynamic *dbt-tv* :root)
(defn dbt-trd [] *dbt-tv*)

(in-ns 'user)

(deftest qualified-binding-frame-unwinds-on-throw
  ;; The frame must be popped on throw; the var root is :root after.
  (try
    (binding [dyn.bind.throw.home/*dbt-tv* :will-unwind]
      (throw :boom))
    (catch _e nil))
  ;; Read via the home-ns fn (unqualified read in home ns) to confirm root.
  (is (= :root (dyn.bind.throw.home/dbt-trd))))

;; ---------------------------------------------------------------------------
;; *ns* save/restore. *ns* is backed by the runtime's current-ns field
;; (reads return it directly), so a `binding` scope must snapshot and
;; restore it -- otherwise an in-ns / ns inside the body leaks past the
;; frame. Mirrors Clojure, where Var thread-binds *ns*.
;; ---------------------------------------------------------------------------

(deftest binding-ns-restore-after-in-ns
  (in-ns 'bind.ns.check)
  (is (= 'bind.ns.check (ns-name *ns*)))
  (binding [*ns* *ns*]
    (in-ns 'user))
  (is (= 'bind.ns.check (ns-name *ns*)))
  (in-ns 'user))

(deftest binding-ns-body-reflects-in-ns-then-restores
  (in-ns 'bind.ns.check2)
  (binding [*ns* *ns*]
    (is (= 'bind.ns.check2 (ns-name *ns*)))
    (in-ns 'user)
    (is (= 'user (ns-name *ns*))))
  (is (= 'bind.ns.check2 (ns-name *ns*)))
  (in-ns 'user))

(deftest binding-ns-nested-restore
  (in-ns 'bind.ns.outer)
  (binding [*ns* *ns*]
    (in-ns 'user)
    (binding [*ns* *ns*]
      (in-ns 'bind.ns.inner)
      (is (= 'bind.ns.inner (ns-name *ns*))))
    (is (= 'user (ns-name *ns*))))
  (is (= 'bind.ns.outer (ns-name *ns*)))
  (in-ns 'user))

(deftest binding-ns-restores-on-throw
  (in-ns 'bind.ns.throw)
  (try
    (binding [*ns* *ns*]
      (in-ns 'user)
      (throw :boom))
    (catch _e nil))
  (is (= 'bind.ns.throw (ns-name *ns*)))
  (in-ns 'user))

(run-tests-and-exit)
