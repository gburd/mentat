(ns clojure.string)

;; The C primitives `clojure.string/lower-case`, `upper-case`,
;; `starts-with?`, `ends-with?`, `includes?`, `replace`, `split`,
;; `join`, and `trim` are installed into this namespace by
;; mino_install_clojure_core. The defs below add nil-handling and
;; arity-coercion wrappers around them and define the rest of the
;; clojure.string surface (capitalize, blank?, escape, triml,
;; trimr, split-lines, trim-newline) on top.

;; Capture primitive references before the wrappers shadow them.
(def ^:private prim-upper-case   upper-case)
(def ^:private prim-lower-case   lower-case)
(def ^:private prim-starts-with? starts-with?)
(def ^:private prim-ends-with?   ends-with?)
(def ^:private prim-includes?    includes?)
(def ^:private prim-replace      -string-replace)
(def ^:private prim-replace-first -string-replace-first)
(def ^:private prim-trim         trim)

(defn- assert-string [s]
  (when-not (string? s)
    (throw (ex-info (str "Argument must be a string, got " (type s)) {:arg s})))
  s)

(defn- as-str [s]
  (if (string? s) s (str s)))

(defn blank? [s]
  (if (nil? s)
    true
    (let [s (assert-string s)]
      (loop [i 0 len (count s)]
        (if (>= i len)
          true
          (let [c (char-at s i)]
            (if (or (= c " ") (= c "\t") (= c "\n") (= c "\r"))
              (recur (+ i 1) len)
              false)))))))

(defn capitalize [s]
  (let [s (as-str s)]
    (if (= s "")
      ""
      (str (prim-upper-case (subs s 0 1))
           (prim-lower-case (subs s 1))))))

(defn starts-with? [s prefix]
  (prim-starts-with? (as-str s) prefix))

(defn ends-with? [s suffix]
  (prim-ends-with? (as-str s) suffix))

(defn escape [s cmap]
  (let [s (assert-string s)]
    (apply str (map (fn [c] (or (get cmap c) c)) (seq s)))))

(defn lower-case [s]
  (prim-lower-case (as-str s)))

(defn upper-case [s]
  (prim-upper-case (as-str s)))

(defn replace
  "Replace all matches of `match` in `s` with `replacement`. `match` may
   be a string, char, or regex. For regex `match`, `replacement` may be
   a string (with `$N` group backrefs and `\\$` / `\\\\` escapes) or a
   function called per match with the whole-match string when the
   pattern has no groups, or `[whole g1 g2 ...]` when it does. Char
   `match` is coerced to a 1-char string before delegating to the C
   primitive."
  [s match replacement]
  (let [s (assert-string s)]
    (cond
      (char? match)
      (prim-replace s (str match) (as-str replacement))

      :else
      ;; The C primitive handles string and regex `match`; for regex it
      ;; also dispatches on whether `replacement` is a string (template
      ;; with backrefs) or a callable.
      (prim-replace s match replacement))))

(defn split-lines [s]
  (split (assert-string s) #"\r?\n"))

(defn triml [s]
  (let [s (assert-string s)
        len (count s)]
    (loop [i 0]
      (if (>= i len)
        ""
        (let [c (char-at s i)]
          (if (or (= c " ") (= c "\t") (= c "\n") (= c "\r"))
            (recur (+ i 1))
            (subs s i)))))))

(defn trimr [s]
  (let [s (assert-string s)
        len (count s)]
    (loop [i len]
      (if (<= i 0)
        ""
        (let [c (char-at s (- i 1))]
          (if (or (= c " ") (= c "\t") (= c "\n") (= c "\r"))
            (recur (- i 1))
            (subs s 0 i)))))))

(defn trim [s]
  (trimr (triml s)))

(defn trim-newline [s]
  (let [s (assert-string s)
        len (count s)]
    (loop [i len]
      (if (<= i 0)
        ""
        (let [c (char-at s (- i 1))]
          (if (or (= c "\n") (= c "\r"))
            (recur (- i 1))
            (subs s 0 i)))))))

(defn includes? [s substr]
  (prim-includes? (assert-string s) (assert-string substr)))

(defn reverse
  "Returns s with its characters reversed."
  [s]
  (apply str (clojure.core/reverse (seq (assert-string s)))))

(defn- index-of-from [s sub from]
  ;; Brute-force substring search. Returns the index of the first
  ;; occurrence of sub in s at or after from, or nil. The hot path
  ;; reuses prim-includes? for the early-out — if sub never appears
  ;; we don't walk character by character.
  (let [s    (assert-string s)
        sub  (as-str sub)
        nlen (count s)
        slen (count sub)]
    (cond
      (zero? slen)        from
      (> (+ from slen) nlen) nil
      (not (prim-includes? (subs s from) sub)) nil
      :else
      (loop [i (max 0 from)]
        (cond
          (> (+ i slen) nlen) nil
          (= (subs s i (+ i slen)) sub) i
          :else (recur (+ i 1)))))))

(defn index-of
  "Return index of value (string or char) in s, optionally searching
   forward from from-index. Returns nil if value not found."
  ([s value]            (index-of-from s value 0))
  ([s value from-index] (index-of-from s value from-index)))

(defn last-index-of
  "Return last index of value (string or char) in s, optionally
   searching backward from from-index. Returns nil if value not found."
  ([s value]
   (last-index-of s value (count (assert-string s))))
  ([s value from-index]
   (let [s     (assert-string s)
         sub   (as-str value)
         slen  (count sub)
         start (min from-index (- (count s) slen))]
     (when (and (>= start 0) (>= slen 0))
       (loop [i start]
         (cond
           (< i 0) nil
           (= (subs s i (+ i slen)) sub) i
           :else (recur (- i 1))))))))

(defn re-quote-replacement
  "Escapes $ and \\ in replacement so s can be used literally in
   replacement strings without triggering backreference syntax."
  [replacement]
  (escape (as-str replacement) {\\ "\\\\" \$ "\\$"}))

(defn replace-first
  "Replaces only the first occurrence of match in s with replacement.
   match may be a string, char, or regex; for regex match, replacement
   may be a $N template string or a function, exactly as in replace."
  [s match replacement]
  (let [s (assert-string s)]
    (if (char? match)
      (prim-replace-first s (str match) (as-str replacement))
      (prim-replace-first s match replacement))))


