(require "tests/test")

;; Arithmetic operations, comparisons, bitwise, and numeric utilities.

(deftest self-eval
  (is (= 42 42))
  (is (= "hi" "hi"))
  (is (= nil nil))
  (is (= true true))
  (is (= false false)))

(deftest addition
  (is (= 3 (+ 1 2)))
  (is (= 3.5 (+ 1 2.5))))

(deftest subtraction
  (is (= 5 (- 10 3 2)))
  (is (= -7 (- 7))))

(deftest multiplication
  (is (= 24 (* 2 3 4))))

(deftest division
  ;; `/` on integers returns a ratio (exact) — use float coercion for a double.
  (is (= 5/2 (/ 10 4)))
  (is (= 2.5 (double (/ 10 4)))))

(deftest comparisons
  (testing "less than"
    (is (< 1 2 3))
    (is (not (< 3 2 1))))
  (testing "equal"
    ;; = is type-strict on numeric tier (Clojure dialect): int and float
    ;; aren't `=` even when numerically equal. Use `==` for cross-tier
    ;; numeric equality.
    (is (not (= 1 1.0)))
    (is (== 1 1.0)))
  (testing "less or equal"
    (is (<= 1 2 2 3)))
  (testing "greater than"
    (is (> 3 2 1))
    (is (not (> 1 2))))
  (testing "greater or equal"
    (is (>= 3 3 1))))

(deftest mod-rem-quot
  (testing "mod"
    (is (= 1 (mod 10 3)))
    (is (= 2 (mod -10 3)))
    (is (= 1.5 (mod 5.5 2.0))))
  (testing "rem"
    (is (= 1 (rem 10 3)))
    (is (= -1 (rem -10 3))))
  (testing "quot"
    (is (= 3 (quot 10 3)))
    (is (= -3 (quot -10 3)))))

(deftest bitwise
  (is (= 8 (bit-and 12 10)))
  (is (= 14 (bit-or 12 10)))
  (is (= 6 (bit-xor 12 10)))
  (is (= -1 (bit-not 0)))
  (is (= 16 (bit-shift-left 1 4)))
  (is (= 4 (bit-shift-right 16 2))))

(deftest bit-shift-boundary
  ;; In-range shifts at both ends of [0, 63] are defined and produce the
  ;; usual two's-complement results.
  (is (= 1 (bit-shift-left 1 0)))
  (is (= -9223372036854775808 (bit-shift-left 1 63)))
  (is (= 1 (bit-shift-right 1 0)))
  (is (= 0 (bit-shift-right 1 63)))
  (is (= 9223372036854775807 (unsigned-bit-shift-right -1 1)))
  (is (= 1 (unsigned-bit-shift-right -1 63)))
  ;; Out-of-range shift amounts (negative or >= 64) must throw; C99 leaves
  ;; such shifts undefined, so surface them as a bounds error rather than
  ;; letting the underlying shift instruction silently mask the count.
  (is (thrown? (bit-shift-left 1 64)))
  (is (thrown? (bit-shift-left 1 -1)))
  (is (thrown? (bit-shift-right 1 64)))
  (is (thrown? (bit-shift-right 1 -1)))
  (is (thrown? (unsigned-bit-shift-right 1 64)))
  (is (thrown? (unsigned-bit-shift-right 1 -1))))

(deftest bit-shift-right-preserves-sign
  ;; bit-shift-right is arithmetic (sign-preserving) on negative inputs,
  ;; matching JVM Clojure. unsigned-bit-shift-right is logical (zero-fill).
  ;; C99 §6.5.7p5 makes `a >> b` implementation-defined for negative a, so
  ;; the implementation must compute the sign-extended result explicitly.
  (is (= -1 (bit-shift-right -1 1)))
  (is (= -64 (bit-shift-right -128 1)))
  (is (= -1 (bit-shift-right -64 63)))
  (is (= 0x0fffffffffffffff
         (bit-shift-right 0x1ffffffffffffffe 1)))
  ;; Same input under unsigned-bit-shift-right must NOT sign-extend.
  (is (= 9223372036854775806 (unsigned-bit-shift-right -4 1)))
  (is (= 1 (unsigned-bit-shift-right -1 63))))

(deftest trivial-compositions
  (is (= 2 (second [1 2 3])))
  (is (= 1 (ffirst '((1 2) (3 4)))))
  (is (= 6 (inc 5)))
  (is (= 4 (dec 5)))
  (is (zero? 0))
  (is (pos? 5))
  (is (neg? -1))
  (is (even? 4))
  (is (odd? 3))
  (is (= 5 (abs -5)))
  (is (= 7 (max 3 7)))
  (is (= 3 (min 3 7))))

(deftest numeric-coercion
  (is (= 3 (int 3.7)))
  (is (= 5 (int 5)))
  ;; `(float x)` returns a 32-bit MINO_FLOAT32 distinct from a
  ;; 64-bit MINO_FLOAT, so equality with a double literal is false
  ;; (matching JVM Clojure where `(= 5.0 (float 5))` is false).
  ;; Same-tier comparisons work normally; `(double x)` widens.
  (is (float? (float 5)))
  (is (false? (double? (float 5))))
  (is (= (float 5) (float 5.0)))
  (is (= 5.0 (double (float 5))))
  (is (NaN? (float ##NaN)))
  (is (thrown? (float ##Inf)))
  (is (thrown? (float ##-Inf))))

(deftest integer-overflow-strict-and-primed
  ;; Plain +/-/*/inc/dec throw on long overflow (matching JVM Clojure's
  ;; unprimed contracts). The primed forms +' / -' / *' / inc' / dec'
  ;; auto-promote to bigint. The unchecked-* family is a separate
  ;; opt-in for two's-complement-wraparound int64 semantics.
  (let [max-int 9223372036854775807
        min-int -9223372036854775808]
    ;; Strict forms throw on overflow.
    (is (thrown? (+ max-int 1)))
    (is (thrown? (+ 1 max-int)))
    (is (thrown? (+ min-int -1)))
    (is (thrown? (+ 1 2 max-int)))
    (is (thrown? (- min-int 1)))
    (is (thrown? (- max-int -1)))
    (is (thrown? (- min-int)))
    (is (thrown? (* max-int 2)))
    (is (thrown? (* -1 min-int)))
    (is (thrown? (* min-int -1)))
    (is (thrown? (inc max-int)))
    (is (thrown? (dec min-int)))
    ;; Primed forms auto-promote.
    (is (= 9223372036854775808N (+' max-int 1)))
    (is (= 9223372036854775808N (+' 1 max-int)))
    (is (= -9223372036854775809N (+' min-int -1)))
    (is (= 9223372036854775810N (+' 1 2 max-int)))
    (is (= -9223372036854775809N (-' min-int 1)))
    (is (= 9223372036854775808N (-' max-int -1)))
    (is (= 9223372036854775808N (-' min-int)))
    (is (= 18446744073709551614N (*' max-int 2)))
    (is (= 9223372036854775808N (*' -1 min-int)))
    (is (= 9223372036854775808N (*' min-int -1)))
    (is (= 9223372036854775808N (inc' max-int)))
    (is (= -9223372036854775809N (dec' min-int)))
    (is (= :bigint (type (+' max-int 1))))
    (is (= :bigint (type (*' max-int 2))))
    ;; In-range arithmetic stays in long for both forms.
    (is (= max-int (+ (dec' max-int) 1)))
    (is (= (- max-int 1) (+ max-int -1)))
    (is (= -1 (+ max-int min-int)))
    (is (= max-int (* max-int 1)))
    (is (= :int (type (+ 1 2))))
    (is (= :int (type (+' 1 2))))
    ;; Float arithmetic is not range-checked; IEEE 754 handles it.
    (is (float? (+ max-int 1.0)))
    ;; unchecked-* wraps without promoting.
    (is (= min-int (unchecked-add max-int 1)))
    (is (= max-int (unchecked-subtract min-int 1)))
    (is (= min-int (unchecked-inc max-int)))
    (is (= max-int (unchecked-dec min-int)))
    (is (= min-int (unchecked-multiply 2 4611686018427387904)))))

(deftest unchecked-narrowing-casts
  ;; unchecked-int/-long/-byte/-short/-char/-float/-double take a number
  ;; and reinterpret it in the target width with two's-complement
  ;; truncation (matching JVM semantics).
  (testing "unchecked-long passes long values through"
    (is (= 1 (unchecked-long 1)))
    (is (= -1 (unchecked-long -1)))
    (is (= 1 (unchecked-long 1.1)))
    (is (= 1 (unchecked-long 1.9)))
    (is (= -1 (unchecked-long -1.9))))
  (testing "unchecked-int truncates to 32-bit signed"
    (is (= 1 (unchecked-int 1)))
    (is (= -1 (unchecked-int -1)))
    (is (= -2147483648 (unchecked-int 2147483648))))
  (testing "unchecked-byte truncates to 8-bit signed"
    (is (= -1 (unchecked-byte 255)))
    (is (= -128 (unchecked-byte 128)))
    (is (= 0 (unchecked-byte 256)))
    (is (= 1 (unchecked-byte 1))))
  (testing "unchecked-short truncates to 16-bit signed"
    (is (= -1 (unchecked-short 65535)))
    (is (= -32768 (unchecked-short 32768)))
    (is (= 1 (unchecked-short 1))))
  (testing "unchecked-char truncates to 16-bit unsigned (char)"
    (is (= \A (unchecked-char 65)))
    (is (= \0 (unchecked-char 48))))
  (testing "unchecked-float narrows to 32-bit float"
    ;; (= 1.0 (float 1)) is false on JVM Clojure too (Float != Double);
    ;; numeric `==` is the cross-tier comparison.
    (is (== 1.0 (unchecked-float 1)))
    (is (== 1.0 (unchecked-float 1.0)))
    (is (float? (unchecked-float 1))))
  (testing "unchecked-double widens to 64-bit double"
    (is (= 1.0 (unchecked-double 1)))
    (is (= 1.0 (unchecked-double 1.0)))))

(deftest unchecked-int-arithmetic
  ;; -int variants do 32-bit two's-complement wraparound; matches JVM
  ;; semantics where the int width is 32. The non-suffixed members of
  ;; the family (unchecked-add / -subtract / -multiply / -inc / -dec)
  ;; use 64-bit wraparound — they are the long-domain opt-in.
  (testing "in-range arithmetic returns the canonical answer"
    (is (= 0 (unchecked-subtract-int 1 1)))
    (is (= 2 (unchecked-add-int 1 1)))
    (is (= 6 (unchecked-multiply-int 2 3)))
    (is (= 1 (unchecked-inc-int 0)))
    (is (= -1 (unchecked-dec-int 0)))
    (is (= -5 (unchecked-negate-int 5)))
    (is (= 1 (unchecked-remainder-int 10 3))))
  (testing "32-bit wraparound at the int boundary"
    (is (= -2147483648 (unchecked-add-int 2147483647 1)))
    (is (= 2147483647  (unchecked-subtract-int -2147483648 1)))
    (is (= -2147483648 (unchecked-inc-int 2147483647)))
    (is (= 2147483647  (unchecked-dec-int -2147483648)))
    (is (= 2147483647  (unchecked-negate-int -2147483647)))
    ;; INT_MIN % -1 is 0 by JVM convention (vs UB in C).
    (is (= 0 (unchecked-remainder-int -2147483648 -1))))
  (testing "division by zero throws"
    (is (thrown? (unchecked-remainder-int 1 0)))))

(deftest tagged-int-boundary
  ;; The internal int representation tags values that fit in 61 signed
  ;; bits (about +-1.15e18) and boxes anything wider up to the long
  ;; range. These tests cross the tagged boundary in every fast-lane
  ;; arith op so the inline encode/decode and the boxed slow path both
  ;; get exercised. Result *values* are what we observe -- the tag bit
  ;; is internal -- but UBSan catches every mis-tagged deref.
  (let [tag-max (- (bit-shift-left 1 60) 1)
        tag-min (- (bit-shift-left 1 60))
        tag-max-plus-1 (+' tag-max 1)
        tag-min-minus-1 (-' tag-min 1)]
    ;; Values right at the tagged boundary stay int.
    (is (= :int (type tag-max)))
    (is (= :int (type tag-min)))
    ;; inc / dec at the boundary cross to boxed but stay :int -- the
    ;; constructor's boxed fallback handles the LLONG band beyond the
    ;; tagged range.
    (is (= tag-max-plus-1 (inc tag-max)))
    (is (= tag-min-minus-1 (dec tag-min)))
    (is (= :int (type (inc tag-max))))
    (is (= :int (type (dec tag-min))))
    ;; + / - / * crossing the boundary in either direction.
    (is (= tag-max-plus-1 (+ tag-max 1)))
    (is (= tag-min-minus-1 (- tag-min 1)))
    (is (= -1 (+ tag-max tag-min-minus-1 1)))
    (is (= tag-max (- tag-max-plus-1 1)))
    (is (= tag-min (+ tag-min-minus-1 1)))
    ;; Multiplication inside the tagged range stays inline.
    (is (= (- tag-max 1) (* 2 (quot (- tag-max 1) 2))))
    ;; Comparison across the boundary is still correct.
    (is (< tag-max tag-max-plus-1))
    (is (> tag-min-minus-1 -1.5e19))
    (is (= tag-max tag-max))
    (is (not= tag-max tag-max-plus-1)))
  ;; INT64_MAX / INT64_MIN boundaries: same overflow rules as JVM
  ;; Clojure (strict + / inc throw on long overflow). The fast lane
  ;; must return NULL on these so the prim slow path runs and throws.
  (let [max-int 9223372036854775807
        min-int -9223372036854775808]
    (is (thrown? (inc max-int)))
    (is (thrown? (dec min-int)))
    (is (thrown? (+ max-int 1)))
    (is (thrown? (- min-int 1)))
    (is (thrown? (* max-int 2)))
    ;; In-range arithmetic still returns the right value.
    (is (= max-int (+ (dec max-int) 1)))
    (is (= min-int (- (inc min-int) 1))))
  ;; ±1 around tagged + int64 boundaries inside a fn so the BC fast
  ;; lane runs (top-level uses the tree-walker).
  (let [tag-max (- (bit-shift-left 1 60) 1)]
    (is (= tag-max ((fn [n] (+ n 0)) tag-max)))
    (is (= (+' tag-max 1) ((fn [n] (+ n 1)) tag-max)))
    (is (= (- tag-max 1) ((fn [n] (dec n)) tag-max)))
    (is (= tag-max ((fn [n] (inc n)) (- tag-max 1))))))

(deftest scientific-notation-signed-exponents
  (is (float? 1e10))
  (is (float? 1e-10))
  (is (float? 1e+10))
  (is (float? 1.5e-3))
  (is (float? -2.5e+4))
  (is (float? 3E-2))
  (is (= 1e10 1e+10))
  (is (< 1e-10 1))
  (is (< 1.5e-3 1))
  (is (< -2.5e+4 0)))
