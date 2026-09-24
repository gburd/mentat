(require "tests/test")

;; Tests from clojure/test_clojure/other_functions.clj
;; Expected values are taken as-is from the Clojure test suite.

;; --- identity ---

(deftest clj-symbol-as-function
  ;; JVM Clojure makes Symbols callable as keyword-style lookups.
  (is (= 1   ('inc {'inc 1 'dec 2})))
  (is (= 2   ('dec {'inc 1 'dec 2})))
  (is (= nil ('missing {'a 1})))
  (is (= 99  ('missing {'a 1} 99)))
  ;; Non-map collections return the default (nil) — matches JVM.
  (is (= '(nil nil nil)
         (map (fn [s] (s 0)) '(inc dec zero?)))))

(deftest clj-identity
  (is (= (identity nil) nil))
  (is (= (identity false) false))
  (is (= (identity true) true))
  (is (= (identity 0) 0))
  (is (= (identity 42) 42))
  (is (= (identity 0.0) 0.0))
  (is (= (identity 3.14) 3.14))
  (is (= (identity "") ""))
  (is (= (identity "abc") "abc"))
  (is (= (identity :kw) :kw))
  (is (= (identity []) []))
  (is (= (identity [1 2]) [1 2]))
  (is (= (identity {}) {}))
  (is (= (identity {:a 1 :b 2}) {:a 1 :b 2}))
  (is (= (identity #{}) #{}))
  (is (= (identity #{1 2}) #{1 2}))
  (is (= (identity (+ 1 2)) 3))
  (is (= (identity (> 5 0)) true)))

;; --- comp ---

(deftest clj-comp
  (testing "comp with no args returns identity"
    (let [c0 (comp)]
      (is (= (identity nil) (c0 nil)))
      (is (= (identity 42) (c0 42)))
      (is (= (identity [1 2 3]) (c0 [1 2 3])))
      (is (= (identity #{}) (c0 #{})))
      (is (= (identity :foo) (c0 :foo)))
      (is (= (identity (+ 1 2 3)) (c0 6)))
      (is (= (identity (keyword "foo")) (c0 :foo))))))

;; --- complement ---

(deftest clj-complement
  (let [not-contains? (complement contains?)]
    (is (= true (not-contains? [2 3 4] 5)))
    (is (= false (not-contains? [2 3 4] 2))))
  (let [first-elem-not-1? (complement (fn [x] (= 1 (first x))))]
    (is (= true (first-elem-not-1? [2 3])))
    (is (= false (first-elem-not-1? [1 2])))))

;; --- constantly ---

(deftest clj-constantly
  (let [c0 (constantly 10)]
    (is (= 10 (c0 nil)))
    (is (= 10 (c0 42)))
    (is (= 10 (c0 "foo")))
    (is (= 10 (c0 nil :a :b :c)))
    (is (= 10 (c0 42 :a :b :c)))
    (is (= 10 (c0 "foo" :a :b :c)))))

;; --- juxt ---

(deftest clj-juxt
  (testing "juxt for colls"
    (let [a0 [1 2]]
      (is (= [1 2] ((juxt :a :b) {:a 1 :b 2})))
      (is (= [2 1] ((juxt fnext first) a0)))))
  (testing "juxt for fns"
    (let [a1 (fn [a] (+ 2 a))
          b1 (fn [b] (* 2 b))]
      (is (= [5 6] ((juxt a1 b1) 3))))))

;; --- partial ---

(deftest clj-partial
  (let [p0 (partial inc)
        p1 (partial + 20)
        p2 (partial conj [1 2])]
    (is (= 41 (p0 40)))
    (is (= 40 (p1 20)))
    (is (= [1 2 3] (p2 3)))))

;; --- fnil ---

(deftest clj-fnil
  (is (= ((fnil + 0) nil 42) 42))
  (is (= ((fnil conj []) nil 42) [42]))
  (is (= (reduce #(update-in %1 [%2] (fnil inc 0)) {}
                  ["fun" "counting" "words" "fun"])
         {"words" 1, "counting" 1, "fun" 2}))
  (is (= (reduce #(update-in %1 [(first %2)] (fnil conj []) (second %2)) {}
                  [[:a 1] [:a 2] [:b 3]])
         {:b [3], :a [1 2]})))

;; --- every-pred ---

(deftest clj-every-pred
  ;; 1 pred
  (is (= true ((every-pred even?))))
  (is (= true ((every-pred even?) 2)))
  (is (= true ((every-pred even?) 2 4)))
  (is (= true ((every-pred even?) 2 4 6)))
  (is (= true ((every-pred even?) 2 4 6 8)))
  (is (= true ((every-pred even?) 2 4 6 8 10)))
  (is (= false ((every-pred odd?) 2)))
  (is (= false ((every-pred odd?) 2 4)))
  (is (= false ((every-pred odd?) 2 4 6)))
  (is (= false ((every-pred odd?) 2 4 6 8)))
  (is (= false ((every-pred odd?) 2 4 6 8 10)))
  ;; 2 preds
  (is (= true ((every-pred even? number?))))
  (is (= true ((every-pred even? number?) 2)))
  (is (= true ((every-pred even? number?) 2 4 6 8 10)))
  (is (= false ((every-pred number? odd?) 2)))
  (is (= false ((every-pred number? odd?) 2 4 6 8 10)))
  ;; 3 preds
  (is (= true ((every-pred even? number? #(> % 0)))))
  (is (= true ((every-pred even? number? #(> % 0)) 2 4 6 8 10)))
  (is (= false ((every-pred number? odd? #(> % 0)) 2 4 6 8 10))))

;; --- some-fn ---

(deftest clj-some-fn
  ;; 1 pred
  (is (not ((some-fn even?))))
  (is ((some-fn even?) 2))
  (is ((some-fn even?) 2 4 6 8 10))
  (is (not ((some-fn odd?) 2)))
  (is (not ((some-fn odd?) 2 4 6 8 10)))
  ;; 2 preds
  (is (not ((some-fn even? number?))))
  (is ((some-fn even? number?) 2))
  (is ((some-fn number? odd?) 2))
  ;; 3 preds
  (is (not ((some-fn even? number? #(> % 0)))))
  (is ((some-fn even? number? #(> % 0)) 2)))

;; --- update ---

(deftest clj-update
  (is (= {:a [1 2]} (update {:a [1]} :a conj 2)))
  (is (= [1] (update [0] 0 inc)))
  (is (= {:a {:b 2}} (update-in {:a {:b 1}} [:a] update :b inc)))
  (is (= {:a 1 :b nil} (update {:a 1} :b identity)))
  (is (= {:a 1} (update {:a 1} :a +)))
  (is (= {:a 2} (update {:a 1} :a + 1)))
  (is (= {:a 3} (update {:a 1} :a + 1 1)))
  (is (= {:a 4} (update {:a 1} :a + 1 1 1)))
  (is (= {:a 5} (update {:a 1} :a + 1 1 1 1)))
  (is (= {:a 6} (update {:a 1} :a + 1 1 1 1 1))))

;; --- find ---

(deftest clj-find
  (is (= (find {} :a) nil))
  (is (= (find {:a 1} :a) [:a 1]))
  (is (= (find {:a 1} :b) nil))
  (is (= (find {nil 1} nil) [nil 1]))
  (is (= (find {:a 1 :b 2} :a) [:a 1]))
  (is (= (find {:a 1 :b 2} :b) [:b 2]))
  (is (= (find {:a 1 :b 2} :c) nil))
  (is (= (find {} nil) nil))
  (is (= (find {:a 1} nil) nil))
  (is (= (find {:a 1 :b 2} nil) nil)))

;; --- get-in ---

(deftest clj-get-in
  (let [m {:a 1 :b 2 :c {:d 3 :e 4} :f nil :g false}]
    (is (= (get-in m [:c :e]) 4))
    (is (= (get-in m [:c :x]) nil))
    (is (= (get-in m [:f]) nil))
    (is (= (get-in m [:g]) false))
    (is (= (get-in m [:h]) nil))
    (is (= (get-in m []) m))
    (is (= (get-in m [:c :e] 0) 4))
    (is (= (get-in m [:c :x] 0) 0))
    (is (= (get-in m [:b] 0) 2))
    (is (= (get-in m [:f] 0) nil))
    (is (= (get-in m [:g] 0) false))
    (is (= (get-in m [:h] 0) 0))
    (is (= (get-in m [:x :y] {:y 1}) {:y 1}))
    (is (= (get-in m [] 0) m))))

;; --- key / val ---

(deftest clj-key-val
  (is (= :a (key (first {:a 1}))))
  (is (= 1 (val (first {:a 1}))))
  (is (= nil (key (first {nil 1}))))
  (is (= 1 (val (first {nil 1}))))
  (is (= :a (key (first {:a nil}))))
  (is (= nil (val (first {:a nil})))))

;; --- vec ---

(deftest clj-vec
  (is (= [0 1 2 3] (vec [0 1 2 3])))
  (is (= [0 1 2 3] (vec (list 0 1 2 3))))
  (is (= [0 1 2 3] (vec (range 4)))))

;; --- empty ---

(deftest clj-empty
  (is (= nil (empty nil)))
  (is (= [] (empty [])))
  (is (= [] (empty [1 2])))
  (is (= {} (empty {})))
  (is (= {} (empty {:a 1 :b 2})))
  (is (= #{} (empty #{})))
  (is (= #{} (empty #{1 2})))
  (is (= nil (empty 42)))
  (is (= nil (empty "abc"))))

;; --- zipmap ---

(deftest clj-zipmap
  (is (= {:a 1 :b 2} (zipmap [:a :b] [1 2])))
  (is (= {:a 1} (zipmap [:a] [1 2])))
  (is (= {:a 1} (zipmap [:a :b] [1])))
  (is (= {} (zipmap [] [1 2])))
  (is (= {} (zipmap [:a :b] [])))
  (is (= {} (zipmap [] []))))
