(require "tests/test")

;; Tests from clojure/test_clojure/control.clj, logic.clj, for.clj
;; Expected values are taken as-is from the Clojure test suite.

;; --- loop/recur ---

(deftest clj-loop-recur
  (is (= 1 (loop [] 1)))
  (is (= 3 (loop [a 1]
              (if (< a 3)
                (recur (inc a))
                a))))
  (is (= [2 4 6] (loop [a []
                         b [1 2 3]]
                    (if (seq b)
                      (recur (conj a (* 2 (first b)))
                             (next b))
                      a))))
  (is (= [6 4 2] (loop [a ()
                         b [1 2 3]]
                    (if (seq b)
                      (recur (conj a (* 2 (first b)))
                             (next b))
                      a)))))

;; --- when ---

(deftest clj-when
  (is (= 1 (when true 1)))
  (is (= nil (when true)))
  (is (= nil (when false)))
  (is (= nil (when false (throw (ex-info "boom" {}))))))

;; --- when-not ---

(deftest clj-when-not
  (is (= 1 (when-not false 1)))
  (is (= nil (when-not true)))
  (is (= nil (when-not false)))
  (is (= nil (when-not true (throw (ex-info "boom" {}))))))

;; --- if-let ---

(deftest clj-if-let
  (is (= 1 (if-let [a 1] a)))
  (is (= 2 (if-let [[a b] '(1 2)] b)))
  (is (= nil (if-let [a false] (throw (ex-info "boom" {})))))
  (is (= 1 (if-let [a false] a 1)))
  (is (= 1 (if-let [[a b] nil] b 1)))
  (is (= 1 (if-let [a false] (throw (ex-info "boom" {})) 1))))

;; --- when-let ---

(deftest clj-when-let
  (is (= 1 (when-let [a 1] a)))
  (is (= 2 (when-let [[a b] '(1 2)] b)))
  (is (= nil (when-let [a false] (throw (ex-info "boom" {}))))))

;; --- cond ---

(deftest clj-cond
  (is (= nil (cond)))
  (is (= nil (cond nil true)))
  (is (= nil (cond false true)))
  (is (= 1 (cond true 1 true (throw (ex-info "boom" {})))))
  (is (= 3 (cond nil 1 false 2 true 3 true 4)))
  (is (= 3 (cond nil 1 false 2 true 3 true (throw (ex-info "boom" {})))))
  ;; false values
  (is (= (cond nil :a true :b) :b))
  (is (= (cond false :a true :b) :b))
  ;; truthy values (0, empty string, etc.)
  (is (= (cond true :a true :b) :a))
  (is (= (cond 0 :a true :b) :a))
  (is (= (cond 42 :a true :b) :a))
  (is (= (cond 0.0 :a true :b) :a))
  (is (= (cond "" :a true :b) :a))
  (is (= (cond "abc" :a true :b) :a))
  (is (= (cond :kw :a true :b) :a))
  (is (= (cond [] :a true :b) :a))
  (is (= (cond [1 2] :a true :b) :a))
  (is (= (cond {} :a true :b) :a))
  (is (= (cond {:a 1} :a true :b) :a))
  (is (= (cond #{} :a true :b) :a))
  (is (= (cond #{1 2} :a true :b) :a))
  ;; evaluation
  (is (= 3 (cond (> 3 2) (+ 1 2) true :result true (throw (ex-info "boom" {})))))
  (is (= :result (cond (< 3 2) (+ 1 2) true :result true (throw (ex-info "boom" {}))))))

;; --- condp ---

(deftest clj-condp
  (is (= :pass (condp = 1  1 :pass  2 :fail)))
  (is (= :pass (condp = 1  2 :fail  1 :pass)))
  (is (= :pass (condp = 1  2 :fail  :pass)))
  (is (= :pass (condp = 1  :pass))))

(deftest clj-condp-arrow-form
  ;; (condp pred expr test :>> result-fn ...) calls result-fn with the
  ;; truthy pred-result whenever the clause matches.
  (is (= 3 (condp some [1 2 3 4]
             #{0 6 7} :>> inc
             #{4 5 9} :>> dec
             :default)))
  (is (= "got true" (condp = 1 1 :>> (fn [x] (str "got " x)) :default)))
  (is (= :default   (condp = 99 1 :>> (fn [x] (str "got " x)) :default))))

;; --- case ---

(deftest clj-case
  (testing "basic matching"
    (let [test-fn (fn [x] (case x 1 :number "foo" :string :zap :keyword :default))]
      (is (= :number (test-fn 1)))
      (is (= :string (test-fn "foo")))
      (is (= :keyword (test-fn :zap)))
      (is (= :default (test-fn :anything-not-appearing-above)))))
  (testing "nil matching"
    (is (= :nil (case nil nil :nil :default))))
  (testing "default"
    (is (= :default (case 999 1 :one :default)))))

;; --- dotimes ---

(deftest clj-dotimes
  (is (= nil (dotimes [n 1] n)))
  (is (= 3 (let [a (atom 0)]
              (dotimes [n 3] (swap! a inc))
              @a)))
  (is (= [0 1 2] (let [a (atom [])]
                    (dotimes [n 3] (swap! a conj n))
                    @a)))
  (is (= [] (let [a (atom [])]
              (dotimes [n 0] (swap! a conj n))
              @a))))

;; --- for ---

(deftest clj-for-when
  (is (= (for [x (range 10) :when (odd? x)] x) '(1 3 5 7 9)))
  (is (= (for [x (range 4) y (range 4) :when (odd? y)] [x y])
         '([0 1] [0 3] [1 1] [1 3] [2 1] [2 3] [3 1] [3 3])))
  (is (= (for [x (range 4) y (range 4) :when (odd? x)] [x y])
         '([1 0] [1 1] [1 2] [1 3] [3 0] [3 1] [3 2] [3 3])))
  (is (= (for [x (range 5) y (range 5) :when (< x y)] [x y])
         '([0 1] [0 2] [0 3] [0 4] [1 2] [1 3] [1 4] [2 3] [2 4] [3 4]))))

(deftest clj-for-while
  (is (= (for [x (range 6) :while (< x 5)] x) '(0 1 2 3 4)))
  (is (= (for [x (range 4) y (range 4) :while (< y 3)] [x y])
         '([0 0] [0 1] [0 2] [1 0] [1 1] [1 2]
           [2 0] [2 1] [2 2] [3 0] [3 1] [3 2])))
  (is (= (for [x (range 4) y (range 4) :while (< x 3)] [x y])
         '([0 0] [0 1] [0 2] [0 3] [1 0] [1 1] [1 2] [1 3]
           [2 0] [2 1] [2 2] [2 3]))))

(deftest clj-for-let
  (is (= (for [x (range 3) y (range 3) :let [z (+ x y)] :when (odd? z)] [x y z])
         '([0 1 1] [1 0 1] [1 2 3] [2 1 3])))
  (is (= (for [x (range 6) :let [y (rem x 2)] :when (even? y) z [8 9]] [x z])
         '([0 8] [0 9] [2 8] [2 9] [4 8] [4 9]))))

(deftest clj-for-nesting
  (is (= (for [x ['a nil] y [x 'b]] [x y])
         '([a a] [a b] [nil nil] [nil b]))))

;; --- and ---

(deftest clj-and
  (is (= true (and)))
  (is (= true (and true)))
  (is (= nil (and nil)))
  (is (= false (and false)))
  (is (= nil (and true nil)))
  (is (= false (and true false)))
  (is (= "abc" (and 1 true :kw 'abc "abc")))
  (is (= nil (and 1 true :kw nil 'abc "abc")))
  (is (= nil (and 1 true :kw nil (throw (ex-info "boom" {})) 'abc "abc")))
  (is (= false (and 1 true :kw 'abc "abc" false)))
  (is (= false (and 1 true :kw 'abc "abc" false (throw (ex-info "boom" {}))))))

;; --- or ---

(deftest clj-or
  (is (= nil (or)))
  (is (= true (or true)))
  (is (= nil (or nil)))
  (is (= false (or false)))
  (is (= true (or nil false true)))
  (is (= 1 (or nil false 1 2)))
  (is (= "abc" (or nil false "abc" :kw)))
  (is (= nil (or false nil)))
  (is (= false (or nil false)))
  (is (= false (or nil nil nil false)))
  (is (= true (or nil true false)))
  (is (= true (or nil true (throw (ex-info "boom" {})) false)))
  (is (= "abc" (or nil false "abc" (throw (ex-info "boom" {}))))))

;; --- not ---

(deftest clj-not
  ;; returns true for nil/false
  (is (= (not nil) true))
  (is (= (not false) true))
  ;; returns false for everything else
  (is (= (not true) false))
  (is (= (not 0) false))
  (is (= (not 0.0) false))
  (is (= (not 42) false))
  (is (= (not 1.2) false))
  (is (= (not "") false))
  (is (= (not "abc") false))
  (is (= (not :kw) false))
  (is (= (not []) false))
  (is (= (not [1 2]) false))
  (is (= (not {}) false))
  (is (= (not {:a 1 :b 2}) false))
  (is (= (not #{}) false))
  (is (= (not #{1 2}) false)))
