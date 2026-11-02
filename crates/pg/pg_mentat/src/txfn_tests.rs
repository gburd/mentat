// Tests for user-defined transaction functions (Codeberg #325): an ident with
// a `:db/fn` (mino source of `(fn [db & args] ...)` returning tx-data) is
// invoked as `[:my/fn arg ...]` inside edn_t and expanded before the
// transactor runs. Gated behind the `script` feature, like edn_eval.

#![cfg(feature = "script")]

#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod tests {
    use pgrx::prelude::*;
    use pgrx::JsonB;

    fn setup() {
        crate::ensure_extension_loaded();
        Spi::run("SELECT bootstrap_schema()").expect("bootstrap_schema failed");
        Spi::run(
            "CREATE OR REPLACE FUNCTION mentat._txfn_err(stmt TEXT) RETURNS TEXT
             LANGUAGE plpgsql AS $$
             BEGIN EXECUTE stmt; RETURN '';
             EXCEPTION WHEN OTHERS THEN RETURN SQLERRM; END; $$",
        )
        .expect("helper");
        t(r#"[{:db/ident :tf/name :db/valueType :db.type/string
                :db/cardinality :db.cardinality/one :db/unique :db.unique/identity}
               {:db/ident :tf/n :db/valueType :db.type/long
                :db/cardinality :db.cardinality/one}]"#);
    }

    fn t(edn: &str) -> String {
        Spi::get_one_with_args::<String>(
            "SELECT edn_t($1)",
            &[pgrx::datum::DatumWithOid::from(edn)],
        )
        .expect("edn_t")
        .expect("NULL")
    }

    fn err(edn: &str) -> String {
        let stmt = format!("SELECT edn_t({})", quote(edn));
        Spi::get_one_with_args::<String>(
            "SELECT mentat._txfn_err($1)",
            &[pgrx::datum::DatumWithOid::from(stmt.as_str())],
        )
        .expect("err helper")
        .unwrap_or_default()
    }

    fn quote(s: &str) -> String {
        format!("'{}'", s.replace('\'', "''"))
    }

    /// Install a tx-fn: an ident whose :db/fn is the given mino fn source.
    fn install(ident: &str, src: &str) {
        let src_edn = format!("{:?}", src); // a Rust debug string is a valid EDN string here
        t(&format!("[{{:db/ident {ident} :db/fn {src_edn}}}]"));
    }

    fn n_of(name: &str) -> Option<i64> {
        let q = format!("[:find ?n . :where [?e :tf/name \"{name}\"] [?e :tf/n ?n]]");
        let j = Spi::get_one_with_args::<JsonB>(
            "SELECT edn_q($1, '{}'::jsonb)",
            &[pgrx::datum::DatumWithOid::from(q.as_str())],
        )
        .expect("edn_q")
        .expect("NULL");
        j.0["result"].as_i64()
    }

    #[pg_test]
    fn txfn_inc_reads_the_db_and_returns_tx_data() {
        setup();
        t(r#"[{:tf/name "ctr" :tf/n 41}]"#);
        // (fn [db e a] ...): read the current value with q, return an add.
        install(
            ":tf/inc",
            "(fn [db name] \
               (let [n (ffirst (mentat.store/q db \
                          '[:find ?n :in $ ?name :where [?e :tf/name ?name] [?e :tf/n ?n]] \
                          name))] \
                 [{:tf/name name :tf/n (inc n)}]))",
        );
        t(r#"[[:tf/inc "ctr"]]"#);
        assert_eq!(n_of("ctr"), Some(42));
        t(r#"[[:tf/inc "ctr"] {:tf/name "other" :tf/n 1}]"#);
        assert_eq!(
            n_of("ctr"),
            Some(43),
            "fn output spliced with other tx-data"
        );
        assert_eq!(n_of("other"), Some(1));
    }

    #[pg_test]
    fn txfn_may_return_builtins_and_call_other_txfns() {
        setup();
        t(r#"[{:tf/name "a" :tf/n 1}]"#);
        // A fn that returns a built-in cas, and a fn that calls that fn.
        install(
            ":tf/set2",
            "(fn [db name] [[:db/cas [:tf/name name] :tf/n 1 2]])",
        );
        install(":tf/outer", "(fn [db name] [[:tf/set2 name]])");
        t(r#"[[:tf/outer "a"]]"#);
        assert_eq!(n_of("a"), Some(2));
        // The cas guard still applies: a second run sees 2, not 1, and fails.
        let e = err(r#"[[:tf/outer "a"]]"#);
        assert!(!e.is_empty(), "cas mismatch must abort the tx");
        assert_eq!(n_of("a"), Some(2), "a failed tx-fn tx changes nothing");
    }

    #[pg_test]
    fn txfn_errors_abort_with_a_clear_message() {
        setup();
        install(":tf/boom", "(fn [db] (throw (ex-info \"nope\" {})))");
        let e = err("[[:tf/boom]]");
        assert!(
            e.contains("txfn-error") && e.contains(":tf/boom"),
            "got: {e}"
        );

        install(":tf/bad", "(fn [db] 42)");
        let e = err("[[:tf/bad]]");
        assert!(e.contains("txfn-result"), "got: {e}");
    }

    #[pg_test]
    fn txfn_self_recursion_is_bounded() {
        setup();
        install(":tf/forever", "(fn [db] [[:tf/forever]])");
        let e = err("[[:tf/forever]]");
        assert!(e.contains("txfn-depth"), "got: {e}");
    }

    #[pg_test]
    fn txfn_is_sandboxed() {
        setup();
        // No host filesystem prims in the tx-fn interpreter (same sandbox as
        // edn_eval): slurp is unbound.
        install(":tf/escape", "(fn [db] (slurp \"/etc/passwd\") [])");
        let e = err("[[:tf/escape]]");
        assert!(
            e.contains("txfn-error"),
            "sandbox must refuse slurp, got: {e}"
        );
    }

    #[pg_test]
    fn unknown_keyword_head_is_left_to_the_transactor() {
        setup();
        // An ident with no :db/fn is not a tx-fn; the transactor's own error
        // (not a txfn error) is reported.
        let e = err("[[:tf/name 1 2]]");
        assert!(!e.contains("txfn"), "got: {e}");
    }

    /// The `:acct/deposit` example in docs/src/datomic-compat.md, verbatim in
    /// shape: a tx-fn that reads with q and returns a lookup-ref cas.
    #[pg_test]
    fn documented_deposit_example_works() {
        setup();
        t(r#"[{:db/ident :acct/id :db/valueType :db.type/string
                :db/cardinality :db.cardinality/one :db/unique :db.unique/identity}
               {:db/ident :acct/balance :db/valueType :db.type/long
                :db/cardinality :db.cardinality/one}]"#);
        t(r#"[{:acct/id "A-1" :acct/balance 100}]"#);
        t(r#"[{:db/ident :acct/deposit
                :db/fn "(fn [db id amt]
                          (let [bal (ffirst (mentat.store/q db
                                      '[:find ?b :in $ ?id :where [?e :acct/id ?id] [?e :acct/balance ?b]]
                                      id))]
                            [[:db/cas [:acct/id id] :acct/balance bal (+ bal amt)]]))"}]"#);
        t(r#"[[:acct/deposit "A-1" 50]]"#);
        let j = Spi::get_one::<JsonB>(
            "SELECT edn_q('[:find ?b . :where [?e :acct/id \"A-1\"] [?e :acct/balance ?b]]', '{}'::jsonb)",
        )
        .expect("q")
        .expect("NULL");
        assert_eq!(j.0["result"].as_i64(), Some(150));
    }

    fn t_on(store: &str, edn: &str) -> String {
        Spi::get_one_with_args::<String>(
            "SELECT t($1, $2)",
            &[
                pgrx::datum::DatumWithOid::from(store),
                pgrx::datum::DatumWithOid::from(edn),
            ],
        )
        .unwrap_or_else(|e| panic!("t({store}, {edn}): {e}"))
        .expect("NULL")
    }

    fn err_on(store: &str, edn: &str) -> String {
        let stmt = format!("SELECT t({}, {})", quote(store), quote(edn));
        Spi::get_one_with_args::<String>(
            "SELECT mentat._txfn_err($1)",
            &[pgrx::datum::DatumWithOid::from(stmt.as_str())],
        )
        .expect("err helper")
        .unwrap_or_default()
    }

    fn n_on(store: &str, name: &str) -> Option<i64> {
        let q = format!("[:find ?n . :where [?e :tf/name \"{name}\"] [?e :tf/n ?n]]");
        Spi::get_one_with_args::<JsonB>(
            "SELECT q($1, $2, '{}'::jsonb)",
            &[
                pgrx::datum::DatumWithOid::from(store),
                pgrx::datum::DatumWithOid::from(q.as_str()),
            ],
        )
        .expect("q")
        .expect("NULL")
        .0["result"]
            .as_i64()
    }

    const INC: &str = "(fn [db name] \
       (let [n (ffirst (mentat.store/q db \
                  '[:find ?n :in $ ?name :where [?e :tf/name ?name] [?e :tf/n ?n]] \
                  name))] \
         [{:tf/name name :tf/n (inc n)}]))";

    /// A named store with setup()'s :tf/* schema.
    fn named(store: &str) {
        Spi::run(&format!("SELECT create_store('{store}')")).expect("create_store");
        t_on(
            store,
            r#"[{:db/ident :tf/name :db/valueType :db.type/string
                 :db/cardinality :db.cardinality/one :db/unique :db.unique/identity}
                {:db/ident :tf/n :db/valueType :db.type/long
                 :db/cardinality :db.cardinality/one}]"#,
        );
    }

    fn install_on(store: &str, ident: &str, src: &str) {
        t_on(store, &format!("[{{:db/ident {ident} :db/fn {src:?}}}]"));
    }

    /// Issue #389: a tx-fn invoked on a named store reads that store. 1.18.0
    /// refused it; before that its `db` read the default store, so with nz's
    /// counter at 1 and the default's at 100, :tf/inc set nz's to 101.
    #[pg_test]
    fn txfn_on_a_named_store_reads_that_store() {
        setup();
        named("nz");
        t_on("nz", r#"[{:tf/name "ctr" :tf/n 1}]"#);
        t(r#"[{:tf/name "ctr" :tf/n 100}]"#);
        install_on("nz", ":tf/inc", INC);

        t_on("nz", r#"[[:tf/inc "ctr"]]"#);
        assert_eq!(n_on("nz", "ctr"), Some(2));
        assert_eq!(n_of("ctr"), Some(100), "the default store is untouched");
        // Next to other tx-data.
        t_on("nz", r#"[{:tf/name "x" :tf/n 5} [:tf/inc "ctr"]]"#);
        assert_eq!(n_on("nz", "ctr"), Some(3));
        assert_eq!(n_on("nz", "x"), Some(5));
        assert_eq!(n_of("x"), None);
        // with() expands against the store too, and commits nothing.
        let j: JsonB = Spi::get_one(r#"SELECT "with"('nz', '[[:tf/inc "ctr"]]')::jsonb"#)
            .expect("with")
            .expect("NULL");
        let asserted_4 = j.0["tx-data"]
            .as_array()
            .expect("tx-data")
            .iter()
            .any(|d| d[2].as_i64() == Some(4) && d[4].as_bool() == Some(true));
        assert!(asserted_4, "with() should assert :tf/n 4: {}", j.0);
        assert_eq!(n_on("nz", "ctr"), Some(3));
        // :tf/inc is defined on nz only: on the default store it's not a
        // tx-fn, so the transactor rejects the call.
        let e = err(r#"[[:tf/inc "ctr"]]"#);
        assert!(!e.is_empty() && !e.contains("txfn-error"), "got: {e}");
        assert_eq!(n_of("ctr"), Some(100));
    }

    /// The same ident defined on two stores runs each store's own definition,
    /// over each store's own data; a tx-fn returning another tx-fn call and a
    /// :db/cas resolves both in the store.
    #[pg_test]
    fn txfn_uses_each_stores_own_definition() {
        setup();
        named("tfa");
        for (store, n) in [("tfa", 10), ("default", 20)] {
            t_on(store, &format!(r#"[{{:tf/name "c" :tf/n {n}}}]"#));
        }
        install_on("tfa", ":tf/bump", "(fn [db name] [[:tf/step name 1]])");
        install_on(
            "default",
            ":tf/bump",
            "(fn [db name] [[:tf/step name 1000]])",
        );
        // :tf/step is a cas on the value the fn read from its own store.
        for store in ["tfa", "default"] {
            install_on(
                store,
                ":tf/step",
                "(fn [db name by] \
                   (let [n (ffirst (mentat.store/q db \
                              '[:find ?n :in $ ?name :where [?e :tf/name ?name] [?e :tf/n ?n]] \
                              name))] \
                     [[:db/cas [:tf/name name] :tf/n n (+ n by)]]))",
            );
        }
        t_on("tfa", r#"[[:tf/bump "c"]]"#);
        t_on("default", r#"[[:tf/bump "c"]]"#);
        assert_eq!(n_on("tfa", "c"), Some(11));
        assert_eq!(n_of("c"), Some(1020));
        // edn_t is the default store, as before.
        t(r#"[[:tf/bump "c"]]"#);
        assert_eq!(n_of("c"), Some(2020));
        assert_eq!(n_on("tfa", "c"), Some(11));
    }

    /// The built-ins run on the store's own tables, next to a user tx-fn.
    #[pg_test]
    fn builtins_on_a_named_store() {
        setup();
        named("tfb");
        t_on(
            "tfb",
            r#"[{:tf/name "ctr" :tf/n 1} {:tf/name "gone" :tf/n 7}]"#,
        );
        t(r#"[{:tf/name "ctr" :tf/n 100}]"#);
        t_on("tfb", r#"[[:db/cas [:tf/name "ctr"] :tf/n 1 2]]"#);
        assert_eq!(n_on("tfb", "ctr"), Some(2));
        t_on("tfb", r#"[[:db.fn/cas [:tf/name "ctr"] :tf/n 2 3]]"#);
        assert_eq!(n_on("tfb", "ctr"), Some(3));
        let e = err_on("tfb", r#"[[:db/cas [:tf/name "ctr"] :tf/n 100 4]]"#);
        assert!(
            !e.is_empty(),
            "cas against the default store's value must fail"
        );
        assert_eq!(n_on("tfb", "ctr"), Some(3));
        t_on("tfb", r#"[[:db/retractEntity [:tf/name "gone"]]]"#);
        assert_eq!(n_on("tfb", "gone"), None);
        t_on("tfb", r#"[{:tf/name "gone2" :tf/n 8}]"#);
        t_on("tfb", r#"[[:db.fn/retractEntity [:tf/name "gone2"]]]"#);
        assert_eq!(n_on("tfb", "gone2"), None);
        assert_eq!(n_of("ctr"), Some(100), "the default store is untouched");
    }
}
