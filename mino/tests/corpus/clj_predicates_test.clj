(require "tests/test")

;; Tests from clojure/test_clojure/predicates.clj
;; Expected values are taken as-is from the Clojure test suite.

;; --- nil? ---

(deftest clj-nil?
  (is (nil? nil))
  (is (not (nil? true)))
  (is (not (nil? false)))
  (is (not (nil? 7)))
  (is (not (nil? :kw)))
  (is (not (nil? "")))
  (is (not (nil? [])))
  (is (not (nil? {})))
  (is (not (nil? #{})))
  (is (not (nil? "abc")))
  (is (not (nil? '(1 2 3))))
  (is (not (nil? [1 2 3])))
  (is (not (nil? {:a 1})))
  (is (not (nil? #{1 2 3})))
  (is (not (nil? (fn [x] (* 2 x))))))

;; --- true? ---

(deftest clj-true?
  (is (true? true))
  (is (not (true? false)))
  (is (not (true? nil)))
  (is (not (true? 7)))
  (is (not (true? 0)))
  (is (not (true? "")))
  (is (not (true? :kw)))
  (is (not (true? [])))
  (is (not (true? {})))
  (is (not (true? #{}))))

;; --- false? ---

(deftest clj-false?
  (is (false? false))
  (is (not (false? true)))
  (is (not (false? nil)))
  (is (not (false? 7)))
  (is (not (false? 0)))
  (is (not (false? "")))
  (is (not (false? :kw)))
  (is (not (false? [])))
  (is (not (false? {})))
  (is (not (false? #{}))))

;; --- boolean? ---

(deftest clj-boolean?
  (is (boolean? true))
  (is (boolean? false))
  (is (not (boolean? 0)))
  (is (not (boolean? 1)))
  (is (not (boolean? -1)))
  (is (not (boolean? 1.0)))
  (is (not (boolean? [])))
  (is (not (boolean? nil)))
  (is (not (boolean? {})))
  (is (not (boolean? :foo))))

;; --- zero? ---

(deftest clj-zero?
  (is (= (zero? 0) true))
  (is (= (zero? 1) false))
  (is (= (zero? -1) false)))

;; --- pos? ---

(deftest clj-pos?
  (is (not (pos? 0)))
  (is (pos? 1))
  (is (not (pos? -1)))
  (is (pos? 1.0))
  (is (not (pos? -1.0))))

;; --- neg? ---

(deftest clj-neg?
  (is (not (neg? 0)))
  (is (not (neg? 1)))
  (is (neg? -1))
  (is (not (neg? 1.0)))
  (is (neg? -1.0)))

;; --- even? ---

(deftest clj-even?
  (is (even? 0))
  (is (not (even? 1)))
  (is (even? 2))
  (is (even? -2))
  (is (not (even? -1))))

;; --- odd? ---

(deftest clj-odd?
  (is (not (odd? 0)))
  (is (odd? 1))
  (is (not (odd? 2)))
  (is (odd? -1))
  (is (not (odd? -2))))

;; --- number? ---

(deftest clj-number?
  (is (number? 0))
  (is (number? 1))
  (is (number? -1))
  (is (number? 1.0))
  (is (not (number? nil)))
  (is (not (number? true)))
  (is (not (number? "abc")))
  (is (not (number? :kw)))
  (is (not (number? [])))
  (is (not (number? {}))))

;; --- integer? ---

(deftest clj-integer?
  (is (integer? 0))
  (is (integer? 1))
  (is (integer? -1))
  (is (not (integer? 1.0)))
  (is (not (integer? nil)))
  (is (not (integer? true)))
  (is (not (integer? "abc")))
  (is (not (integer? :kw)))
  (is (not (integer? []))))

;; --- float? ---

(deftest clj-float?
  (is (float? 1.0))
  (is (float? -1.0))
  (is (float? 0.0))
  (is (not (float? 1)))
  (is (not (float? nil)))
  (is (not (float? true)))
  (is (not (float? "abc")))
  (is (not (float? :kw)))
  (is (not (float? []))))

;; --- string? ---

(deftest clj-string?
  (is (string? ""))
  (is (string? "abc"))
  (is (not (string? nil)))
  (is (not (string? true)))
  (is (not (string? false)))
  (is (not (string? 7)))
  (is (not (string? :kw)))
  (is (not (string? [])))
  (is (not (string? {})))
  (is (not (string? #{})))
  (is (not (string? (fn [x] x)))))

;; --- keyword? ---

(deftest clj-keyword?
  (is (keyword? :kw))
  (is (not (keyword? nil)))
  (is (not (keyword? true)))
  (is (not (keyword? false)))
  (is (not (keyword? 7)))
  (is (not (keyword? "")))
  (is (not (keyword? "abc")))
  (is (not (keyword? [])))
  (is (not (keyword? {})))
  (is (not (keyword? #{}))))

;; --- symbol? ---

(deftest clj-symbol?
  (is (not (symbol? nil)))
  (is (not (symbol? true)))
  (is (not (symbol? false)))
  (is (not (symbol? 7)))
  (is (not (symbol? :kw)))
  (is (not (symbol? "")))
  (is (not (symbol? "abc")))
  (is (not (symbol? [])))
  (is (not (symbol? {})))
  (is (not (symbol? #{}))))

;; --- fn? ---

(deftest clj-fn?
  (is (fn? (fn [x] (* 2 x))))
  (is (not (fn? nil)))
  (is (not (fn? true)))
  (is (not (fn? false)))
  (is (not (fn? 7)))
  (is (not (fn? :kw)))
  (is (not (fn? "")))
  (is (not (fn? [])))
  (is (not (fn? {})))
  (is (not (fn? #{}))))

;; --- map? ---

(deftest clj-map?
  (is (map? {}))
  (is (map? {:a 1 :b 2 :c 3}))
  (is (not (map? nil)))
  (is (not (map? true)))
  (is (not (map? 7)))
  (is (not (map? :kw)))
  (is (not (map? "")))
  (is (not (map? '(1 2 3))))
  (is (not (map? [])))
  (is (not (map? [1 2 3])))
  (is (not (map? #{})))
  (is (not (map? #{1 2 3}))))

;; --- vector? ---

(deftest clj-vector?
  (is (vector? []))
  (is (vector? [1 2 3]))
  (is (not (vector? nil)))
  (is (not (vector? true)))
  (is (not (vector? 7)))
  (is (not (vector? :kw)))
  (is (not (vector? "")))
  (is (not (vector? '(1 2 3))))
  (is (not (vector? {})))
  (is (not (vector? {:a 1})))
  (is (not (vector? #{})))
  (is (not (vector? #{1 2 3}))))

;; --- set? ---

(deftest clj-set?
  (is (set? #{}))
  (is (set? #{1 2 3}))
  (is (not (set? nil)))
  (is (not (set? true)))
  (is (not (set? 7)))
  (is (not (set? :kw)))
  (is (not (set? "")))
  (is (not (set? '(1 2 3))))
  (is (not (set? [])))
  (is (not (set? [1 2 3])))
  (is (not (set? {})))
  (is (not (set? {:a 1}))))

;; --- list? ---

(deftest clj-list?
  (is (list? '(1 2 3)))
  (is (not (list? (lazy-seq [1 2 3]))))
  (is (not (list? nil)))
  (is (not (list? true)))
  (is (not (list? 7)))
  (is (not (list? :kw)))
  (is (not (list? "")))
  (is (not (list? [])))
  (is (not (list? [1 2 3])))
  (is (not (list? {})))
  (is (not (list? {:a 1})))
  (is (not (list? #{})))
  (is (not (list? #{1 2 3}))))

;; --- seq? ---

(deftest clj-seq?
  (is (seq? '(1 2 3)))
  (is (seq? (lazy-seq [1 2 3])))
  (is (not (seq? nil)))
  (is (not (seq? true)))
  (is (not (seq? 7)))
  (is (not (seq? :kw)))
  (is (not (seq? "")))
  (is (not (seq? [])))
  (is (not (seq? [1 2 3])))
  (is (not (seq? {})))
  (is (not (seq? {:a 1})))
  (is (not (seq? #{})))
  (is (not (seq? #{1 2 3}))))

;; --- coll? ---

(deftest clj-coll?
  (is (coll? '(1 2 3)))
  (is (coll? (lazy-seq [1 2 3])))
  (is (coll? []))
  (is (coll? [1 2 3]))
  (is (coll? {}))
  (is (coll? {:a 1}))
  (is (coll? #{}))
  (is (coll? #{1 2 3}))
  (is (not (coll? nil)))
  (is (not (coll? true)))
  (is (not (coll? 7)))
  (is (not (coll? :kw)))
  (is (not (coll? "")))
  (is (not (coll? (fn [x] x)))))

;; --- some? ---

(deftest clj-some?
  (is (some? 0))
  (is (some? false))
  (is (some? ""))
  (is (some? []))
  (is (some? :kw))
  (is (not (some? nil))))
