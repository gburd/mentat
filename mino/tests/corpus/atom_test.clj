(require "tests/test")

;; Atoms: mutable references.

(deftest atom-deref
  (is (= 42 (deref (atom 42))))
  (is (= 42 @(atom 42))))

(deftest atom-reset
  (let [a (atom 0)]
    (reset! a 10)
    (is (= 10 @a))))

(deftest atom-swap
  (let [a (atom 0)]
    (swap! a + 1)
    (is (= 1 @a))))

(deftest atom-swap-extra-args
  (let [a (atom 10)]
    (swap! a + 5 3)
    (is (= 18 @a))))

(deftest atom-predicates
  (is (atom? (atom nil)))
  (is (not (atom? 42))))

(deftest atom-identity
  (let [a (atom 1)]
    (is (= a a)))
  (let [a (atom 1) b (atom 1)]
    (is (not= a b))))

(deftest atom-type
  (is (= :atom (type (atom 1)))))

(deftest atom-nested
  (let [a (atom (atom 1))]
    (is (= 1 @@a))))

(deftest atom-nil
  (is (= nil @(atom nil))))

(deftest atom-swap-conj
  (let [a (atom [1 2])]
    (swap! a conj 3)
    (is (= [1 2 3] @a))))

;; Note: (deref 42) produces a C-level runtime error, not a catchable
;; throw. C runtime errors are tested at the integration level, not here.
