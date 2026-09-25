(require "tests/test")
(require '[clojure.string :as str])

;; --- blank? ---

(deftest str-blank-empty
  (is (true? (str/blank? "")))
  (is (true? (str/blank? nil)))
  (is (true? (str/blank? "  ")))
  (is (true? (str/blank? " \t "))))

(deftest str-blank-non-blank
  (is (false? (str/blank? "hello")))
  (is (false? (str/blank? " x ")))
  (is (false? (str/blank? "nil"))))

(deftest str-blank-throws-non-string
  (is (thrown? (str/blank? 1)))
  (is (thrown? (str/blank? :a)))
  (is (thrown? (str/blank? 'a))))

;; --- capitalize ---

(deftest str-capitalize-basic
  (is (= "Hello world" (str/capitalize "hello WORLD")))
  (is (= "A thing" (str/capitalize "a Thing")))
  (is (= "A thing" (str/capitalize "A THING")))
  (is (= "" (str/capitalize "")))
  (is (= "A" (str/capitalize "a"))))

(deftest str-capitalize-coerces
  (is (= "" (str/capitalize nil)))
  (is (= "1" (str/capitalize 1))))

;; --- starts-with? ---

(deftest str-starts-with-basic
  (is (true? (str/starts-with? "hello" "hel")))
  (is (true? (str/starts-with? "hello" "")))
  (is (false? (str/starts-with? "hello" "world"))))

(deftest str-starts-with-coerces
  (is (false? (str/starts-with? nil "x")))
  (is (false? (str/starts-with? 1 "x"))))

;; --- ends-with? ---

(deftest str-ends-with-basic
  (is (true? (str/ends-with? "hello" "llo")))
  (is (true? (str/ends-with? "hello" "")))
  (is (false? (str/ends-with? "hello" "world"))))

(deftest str-ends-with-coerces
  (is (false? (str/ends-with? nil "x")))
  (is (false? (str/ends-with? 1 "x"))))

;; --- escape ---

(deftest str-escape-basic
  (is (= "" (str/escape "" {})))
  (is (= "A_Abc" (str/escape "abc" {\a "A_A"})))
  (is (= "A_AbC_C" (str/escape "abc" {\a "A_A" \c "C_C"}))))

(deftest str-escape-throws
  (is (thrown? (str/escape nil {\a "A"})))
  (is (thrown? (str/escape 1 {\a "A"}))))

;; --- lower-case ---

(deftest str-lower-case-basic
  (is (= "hello" (str/lower-case "HELLO")))
  (is (= "" (str/lower-case ""))))

(deftest str-lower-case-coerces
  (is (= "" (str/lower-case nil)))
  (is (= "1" (str/lower-case 1))))

;; --- upper-case ---

(deftest str-upper-case-basic
  (is (= "HELLO" (str/upper-case "hello")))
  (is (= "" (str/upper-case ""))))

(deftest str-upper-case-coerces
  (is (= "" (str/upper-case nil)))
  (is (= "1" (str/upper-case 1))))

;; --- reverse ---

(deftest str-reverse-basic
  (is (= "olleh" (str/reverse "hello")))
  (is (= "" (str/reverse "")))
  (is (= "a" (str/reverse "a")))
  (is (= "tset-a" (str/reverse "a-test"))))

(deftest str-reverse-throws
  (is (thrown? (str/reverse nil)))
  (is (thrown? (str/reverse 1)))
  (is (thrown? (str/reverse :a))))

;; --- replace ---

(deftest str-split-limit
  ;; limit 0 (or absent) trims trailing empty pieces; a negative limit
  ;; keeps them; a positive limit caps the piece count with the final
  ;; piece absorbing the rest.
  (is (= ["" "" "a"]       (str/split ",,a,," #",")))
  (is (= ["" "" "a" "" ""] (str/split ",,a,," #"," -1)))
  (is (= ["" "" "a"]       (str/split ",,a,," "," 0)))
  (is (= ["" "" "a" "" ""] (str/split ",,a,," "," -1)))
  (is (= ["" ",a,,"]       (str/split ",,a,," #"," 2)))
  (is (= ["q"]             (str/split "q" #","))))

(deftest str-split-zero-width
  ;; A zero-width separator match contributes a piece boundary without
  ;; consuming input; the leading zero-width match produces no empty
  ;; first piece. An empty pattern therefore splits into characters.
  (is (= ["a" "b" "c"] (str/split "abc" #"")))
  (is (= ["a" "b" "c"] (str/split "abc" "")))
  (is (= ["h" "é" "l"] (str/split "hél" "")))
  (is (= ["h" "é" "l"] (str/split "hél" #"")))
  (is (= ["a" "bc"]    (str/split "abc" #"" 2)))
  (is (= ["a" "b" "c"] (str/split "abc" #"x*y?"))))

(deftest str-split-lines-crlf
  (is (= ["a" "b" "c"] (str/split-lines "a\nb\r\nc")))
  (is (= ["a" "b"] (str/split-lines "a\r\nb")))
  (is (= ["ab"] (str/split-lines "ab"))))

(deftest str-replace-string-match
  (is (= "hello-world" (str/replace "hello world" " " "-")))
  (is (= "abc"         (str/replace "a.b.c" "." "")))
  (is (= ""            (str/replace "" "x" "y"))))

(deftest str-replace-char-match
  (is (= "aXc" (str/replace "abc" \b \X)))
  (is (= "aXc" (str/replace "abc" \b "X"))))

(deftest str-replace-regex-string
  (is (= "aXc"         (str/replace "abc" #"b" "X")))
  (is (= "Hell0 W0rld" (str/replace "Hello World" #"o" "0")))
  (is (= "XbX"         (str/replace "abc" #"[ac]" "X")))
  (is (= "ab"          (str/replace "abc" #"c$" "")))
  (is (= "xbc"         (str/replace "abc" #"^a" "x"))))

(deftest str-replace-regex-backref
  (is (= "[H][e][l][l][o]" (str/replace "Hello" #"(\w)" "[$1]")))
  (is (= "1a2b"            (str/replace "a1b2" #"(\w)(\d)" "$2$1"))))

(deftest str-replace-first-string-match
  (is (= "a!b.c" (str/replace-first "a.b.c" "." "!")))
  (is (= "aXc"   (str/replace-first "abc" \b "X")))
  (is (= "abc"   (str/replace-first "abc" "x" "y"))))

(deftest str-replace-first-regex
  (is (= "heLlo"   (str/replace-first "hello" #"l" "L")))
  (is (= "he[l]lo" (str/replace-first "hello" #"(l)" "[$1]")))
  (is (= "heXlo"   (str/replace-first "hello" #"l" (fn [m] "X"))))
  (is (= "1a-b2"   (str/replace-first "a1-b2" #"(\w)(\d)"
                                      (fn [[_ g1 g2]] (str g2 g1)))))
  (is (= "abc"     (str/replace-first "abc" #"x" "y"))))

(deftest str-replace-regex-quote
  (is (= "$out"  (str/replace "Xout" #"X" (str/re-quote-replacement "$"))))
  (is (= "\\out" (str/replace "Xout" #"X" (str/re-quote-replacement "\\"))))
  (is (= "a\\$1\\\\b" (str/re-quote-replacement "a$1\\b")))
  (is (= "$1x" (str/replace "Ax" #"A" (str/re-quote-replacement "$1")))))

(deftest str-replace-regex-fn
  (is (= "ABC"       (str/replace "abc" #"\w" (fn [m] (str/upper-case m)))))
  (is (= "<a>-<bb>"  (str/replace "a-bb" #"\w+" (fn [m] (str "<" m ">")))))
  (is (= "1a-2b"     (str/replace "a1-b2" #"(\w)(\d)"
                                  (fn [[_ g1 g2]] (str g2 g1))))))
