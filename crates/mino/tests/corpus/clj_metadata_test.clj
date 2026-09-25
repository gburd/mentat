(require "tests/test")

;; Tests from clojure/test_clojure/metadata.clj
;; Expected values are taken as-is from the Clojure test suite.
;; Metadata preservation through collection operations.

;; --- metadata on maps ---

(deftest clj-meta-maps
  (let [xm {:a 1 :b -7}
        x (with-meta {:foo 1 :bar 2} xm)
        ym {:c "foo"}
        y (with-meta {:baz 4 :guh x} ym)]
    (is (= xm (meta (:guh y))))
    (is (= xm (meta (reduce #(assoc %1 %2 (inc %2)) x (range 1000)))))
    (is (= xm (meta (-> x (dissoc :foo) (dissoc :bar)))))
    (let [z (assoc-in y [:guh :la] 18)]
      (is (= ym (meta z)))
      (is (= xm (meta (:guh z)))))
    (let [z (update-in y [:guh :bar] inc)]
      (is (= ym (meta z)))
      (is (= xm (meta (:guh z)))))
    (is (= xm (meta (get-in y [:guh]))))
    (is (= xm (meta (into x y))))
    (is (= ym (meta (into y x))))
    (is (= xm (meta (merge x y))))
    (is (= ym (meta (merge y x))))
    (is (= xm (meta (merge-with + x y))))
    (is (= ym (meta (merge-with + y x))))
    (is (= xm (meta (select-keys x [:bar]))))))

;; --- metadata on vectors ---

(deftest clj-meta-vectors
  (let [xm {:a 1 :b -7}
        x (with-meta [1 2 3] xm)
        ym {:c "foo"}
        y (with-meta [4 x 6] ym)]
    (is (= xm (meta (nth y 1))))
    (is (= xm (meta (assoc x 1 "one"))))
    (is (= xm (meta (reduce #(conj %1 %2) x (range 1000)))))
    (is (= xm (meta (pop (pop (pop x))))))
    (let [z (assoc-in y [1 2] 18)]
      (is (= ym (meta z)))
      (is (= xm (meta (nth z 1)))))
    (let [z (update-in y [1 2] inc)]
      (is (= ym (meta z)))
      (is (= xm (meta (nth z 1)))))
    (is (= xm (meta (get-in y [1]))))
    (is (= xm (meta (into x y))))
    (is (= ym (meta (into y x))))
    (is (= xm (meta (replace {2 "two"} x))))
    (is (= [1 "two" 3] (replace {2 "two"} x)))))

;; --- metadata on sets ---

(deftest clj-meta-sets
  (let [xm {:a 1 :b -7}
        x (with-meta #{1 2 3} xm)
        ym {:c "foo"}
        y (with-meta #{4 x 6} ym)]
    (is (= xm (meta (reduce #(conj %1 %2) x (range 1000)))))
    (is (= xm (meta (-> x (disj 1) (disj 2) (disj 3)))))
    (is (= xm (meta (into x y))))
    (is (= ym (meta (into y x))))))
