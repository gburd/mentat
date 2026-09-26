(require "tests/test")

;; Tests from clojure/test_clojure/numbers.clj and math.clj
;; Expected values are taken as-is from the Clojure test suite.

;; --- + ---

(deftest clj-plus
  (is (= 0 (+)))
  (is (= 1 (+ 1)))
  (is (= 3 (+ 1 2)))
  (is (= 6 (+ 1 2 3)))
  (is (= -1 (+ -1)))
  (is (= -3 (+ -1 -2)))
  (is (= -2 (+ -1 +2 -3)))
  (is (= 0 (+ 1 -1)))
  (is (= 0 (+ -1 1))))

;; --- - ---

(deftest clj-minus
  (is (= -1 (- 1)))
  (is (= -1 (- 1 2)))
  (is (= -4 (- 1 2 3)))
  (is (= 2 (- -2)))
  (is (= 3 (- 1 -2)))
  (is (= 6 (- 1 -2 -3)))
  (is (= 0 (- 1 1)))
  (is (= 0 (- -1 -1))))

;; --- * ---

(deftest clj-multiply
  (is (= 1 (*)))
  (is (= 2 (* 2)))
  (is (= 6 (* 2 3)))
  (is (= 24 (* 2 3 4)))
  (is (= -2 (* -2)))
  (is (= -6 (* 2 -3)))
  (is (= 6 (* 2 -3 -1))))

;; --- / ---

(deftest clj-divide
  (is (= 2 (/ 4 2)))
  (is (= 4 (/ 24 3 2)))
  (is (= -4 (/ 24 3 2 -1)))
  ;; Reciprocal of an integer is a ratio (Clojure dialect).
  (is (= 1/2 (/ 2)))
  (is (= 0.5 (/ 2.0)))
  (is (= 1.5 (/ 4.5 3)))
  (is (= 0.5 (/ 4.5 3.0 3.0))))

;; --- mod ---

(deftest clj-mod
  (is (= 0 (mod 4 2)))
  (is (= 1 (mod 3 2)))
  (is (= 2 (mod 6 4)))
  (is (= 0 (mod 0 5)))
  (is (= 0.0 (mod 4.0 2.0)))
  (is (= 0.5 (mod 4.5 2.0)))
  (is (= 2 (mod 42 5)))
  (is (= -3 (mod 42 -5)))
  (is (= 3 (mod -42 5)))
  (is (= -2 (mod -42 -5)))
  (is (= 0 (mod 9 3)))
  (is (= 0 (mod 9 -3)))
  (is (= 0 (mod -9 3)))
  (is (= 0 (mod -9 -3)))
  (is (= 2 (mod 2 5)))
  (is (= -3 (mod 2 -5)))
  (is (= 3 (mod -2 5)))
  (is (= -2 (mod -2 -5)))
  (is (= 0 (mod 0 3)))
  (is (= 0 (mod 0 -3))))

;; --- rem ---

(deftest clj-rem
  (is (= 0 (rem 4 2)))
  (is (= 1 (rem 3 2)))
  (is (= 2 (rem 6 4)))
  (is (= 0 (rem 0 5)))
  (is (= 0.0 (rem 4.0 2.0)))
  (is (= 0.5 (rem 4.5 2.0)))
  (is (= 2 (rem 42 5)))
  (is (= 2 (rem 42 -5)))
  (is (= -2 (rem -42 5)))
  (is (= -2 (rem -42 -5)))
  (is (= 0 (rem 9 3)))
  (is (= 0 (rem 9 -3)))
  (is (= 0 (rem -9 3)))
  (is (= 0 (rem -9 -3)))
  (is (= 2 (rem 2 5)))
  (is (= 2 (rem 2 -5)))
  (is (= -2 (rem -2 5)))
  (is (= -2 (rem -2 -5)))
  (is (= 0 (rem 0 3)))
  (is (= 0 (rem 0 -3))))

;; --- quot ---

(deftest clj-quot
  (is (= 2 (quot 4 2)))
  (is (= 1 (quot 3 2)))
  (is (= 1 (quot 6 4)))
  (is (= 0 (quot 0 5)))
  (is (= 2.0 (quot 4.0 2.0)))
  (is (= 2.0 (quot 4.5 2.0)))
  (is (= 8 (quot 42 5)))
  (is (= -8 (quot 42 -5)))
  (is (= -8 (quot -42 5)))
  (is (= 8 (quot -42 -5)))
  (is (= 3 (quot 9 3)))
  (is (= -3 (quot 9 -3)))
  (is (= -3 (quot -9 3)))
  (is (= 3 (quot -9 -3)))
  (is (= 0 (quot 2 5)))
  (is (= 0 (quot 2 -5)))
  (is (= 0 (quot -2 5)))
  (is (= 0 (quot -2 -5)))
  (is (= 0 (quot 0 3)))
  (is (= 0 (quot 0 -3))))

;; --- min / max ---

(deftest clj-min-max
  (is (= (min 0.0) 0.0))
  (is (= (max 0.0) 0.0))
  (is (= (min 0.0 -1.0) -1.0))
  (is (= (max 0.0 -1.0) 0.0))
  (is (= (min -1.0 0.0) -1.0))
  (is (= (max -1.0 0.0) 0.0))
  (is (= (min 0.0 1.0) 0.0))
  (is (= (max 0.0 1.0) 1.0))
  (is (= (min 1.0 0.0) 0.0))
  (is (= (max 1.0 0.0) 1.0))
  (is (= (min 0.0 1.0 -1.0) -1.0))
  (is (= (max 0.0 1.0 -1.0) 1.0))
  (is (= (min 0.0 -1.0 1.0) -1.0))
  (is (= (max 0.0 -1.0 1.0) 1.0))
  (is (= (min -1.0 1.0 0.0) -1.0))
  (is (= (max -1.0 1.0 0.0) 1.0)))

;; --- abs ---

(deftest clj-abs
  (is (= 1 (abs -1)))
  (is (= 1 (abs 1)))
  (is (= 1.0 (abs -1.0)))
  (is (= 0.0 (abs -0.0))))

;; --- bit-shift-left ---

(deftest clj-bit-shift-left
  (is (= 2 (bit-shift-left 1 1)))
  (is (= 4 (bit-shift-left 1 2)))
  (is (= 8 (bit-shift-left 1 3)))
  (is (= 46 (bit-shift-left 23 1))))

;; --- bit-shift-right ---

(deftest clj-bit-shift-right
  (is (= 0 (bit-shift-right 1 1)))
  (is (= 2 (bit-shift-right 4 1)))
  (is (= 1 (bit-shift-right 4 2)))
  (is (= 0 (bit-shift-right 4 3)))
  (is (= 11 (bit-shift-right 23 1))))

;; --- bit-clear ---

(deftest clj-bit-clear
  (is (= 13 (bit-clear 15 1)))
  (is (= 13 (bit-clear 13 1))))

;; --- bit-set ---

(deftest clj-bit-set
  (is (= 15 (bit-set 15 1)))
  (is (= 15 (bit-set 13 1))))

;; --- bit-flip ---

(deftest clj-bit-flip
  (is (= 13 (bit-flip 15 1)))
  (is (= 15 (bit-flip 13 1))))

;; --- bit-test ---

(deftest clj-bit-test
  (is (true? (bit-test 15 1)))
  (is (false? (bit-test 13 1))))
