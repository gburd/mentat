(require "tests/test")

;; are macro tests

(deftest are-equality
  (are [x y] (= x y)
    1 1
    2 2
    "a" "a"
    :k :k))

(deftest are-with-computation
  (are [in ex] (= ex (inc in))
    0  1
    1  2
    9  10
    -1 0))

(deftest are-three-bindings
  (are [x y z] (= z (+ x y))
    1  2  3
    10 20 30
    0  0  0))

(deftest are-predicates
  (are [x] (number? x)
    1
    2.5
    -3
    0))

(deftest are-p-thrown
  (are [x] (thrown? (nth [] x))
    0
    1
    99))

;; is must isolate per-assertion errors. An unexpected throw inside the
;; value-extraction part of (is (= a b)) or (is x) must be reported as
;; an error and the surrounding deftest must continue with the next is
;; form. Matches JVM clojure.test's per-assertion isolation contract.

(deftest is-eq-continues-after-throw-in-value
  (let [counters (atom {:pass 0 :fail 0 :error 0 :failures []})]
    (try
      (binding [*report-counters*  counters
                *current-test*     "synthetic"
                *testing-contexts* ()]
        (is (= 0 (throw "boom")))
        (is (= 1 1))
        (is (= 2 2)))
      (catch _ nil))
    (let [s @counters]
      (is (= 2 (:pass s)))
      (is (= 1 (:error s))))))

(deftest is-truthy-continues-after-throw-in-value
  (let [counters (atom {:pass 0 :fail 0 :error 0 :failures []})]
    (try
      (binding [*report-counters*  counters
                *current-test*     "synthetic"
                *testing-contexts* ()]
        (is (throw "boom"))
        (is true)
        (is :keyword))
      (catch _ nil))
    (let [s @counters]
      (is (= 2 (:pass s)))
      (is (= 1 (:error s))))))
