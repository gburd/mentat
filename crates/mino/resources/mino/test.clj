;; test.clj — delegates to lib/clojure/test.clj.
;; Load via (require "tests/test"). Brings clojure.test publics
;; into the caller's namespace so test files can use deftest, is,
;; testing, are, run-tests unqualified without their own (ns ...) form.
(require '[clojure.test :refer :all])
