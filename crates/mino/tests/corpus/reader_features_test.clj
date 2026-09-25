(require "tests/test")
(require '[mino.deps :as deps])

;; Per-dependency reader features: a dep spec may carry
;; :reader-features ["clj"], and files loaded from that dependency's
;; source roots then match the listed reader-conditional feature
;; keywords in addition to :mino and :default. Roots without the tag
;; keep the default behavior: only :mino and :default match. The
;; extra features are scoped to each file's own load, so a tagged
;; file requiring a file under an untagged root does not carry its
;; features into that load.

(def test-dir "/tmp/mino-reader-features-test")

(mkdir-p (str test-dir "/tagged/src/rf_tagged"))
(mkdir-p (str test-dir "/untagged/src/rf_untagged"))
(spit (str test-dir "/tagged/src/rf_tagged/core.cljc")
      (str "(ns rf-tagged.core)\n"
           "(def branch #?(:clj :clj-branch :cljs :cljs-branch))\n"
           "(def splice-probe [:before #?(:cljs :never) :after])\n"
           "(def dialect-first #?(:mino :mino-branch :clj :clj-branch))\n"
           "(def clause-order #?(:clj :clj-branch :mino :mino-branch))\n"
           "(require 'rf-untagged.core)\n"))
(spit (str test-dir "/untagged/src/rf_untagged/core.cljc")
      (str "(ns rf-untagged.core)\n"
           "(def probe [:a #?(:clj 1 :cljs 2) :b])\n"))

(add-load-path! (str test-dir "/tagged/src"))
(add-load-path! (str test-dir "/untagged/src"))
(deps/register-reader-features!
  {:deps {:tagged {:path (str test-dir "/tagged/src")
                   :reader-features ["clj"]}}})
(require 'rf-tagged.core)
(rm-rf test-dir)

(deftest tagged-root-matches-listed-feature
  (is (= :clj-branch rf-tagged.core/branch))
  (is (= [:before :after] rf-tagged.core/splice-probe)))

(deftest untagged-root-keeps-default-features
  ;; Loaded via the require inside the tagged file, so this also
  ;; pins that a tagged load does not leak its features into a
  ;; nested load from an untagged root.
  (is (= [:a :b] rf-untagged.core/probe)))

(deftest first-matching-clause-wins-in-clause-order
  (is (= :mino-branch rf-tagged.core/dialect-first))
  (is (= :clj-branch rf-tagged.core/clause-order)))

(deftest reader-features-spec-validation
  (is (nil? (deps/validate-dep-spec :x {:path "p" :reader-features ["clj"]})))
  (is (thrown? (deps/validate-dep-spec :x {:path "p" :reader-features "clj"})))
  (is (thrown? (deps/validate-dep-spec :x {:path "p" :reader-features [:clj]})))
  (is (thrown? (deps/validate-dep-spec :x {:path "p" :reader-features [""]}))))

(deftest add-reader-features-rejects-bad-arguments
  (is (thrown? (add-reader-features! 42 ["clj"])))
  (is (thrown? (add-reader-features! "root" "clj")))
  (is (thrown? (add-reader-features! "root" [1 2])))
  (is (thrown? (add-reader-features! "" ["clj"]))))

(run-tests-and-exit)
