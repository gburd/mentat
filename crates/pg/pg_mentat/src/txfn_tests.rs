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

    /// The SQLERRM of `sql`, or '' when it succeeds.
    fn err_sql(sql: &str) -> String {
        Spi::get_one_with_args::<String>(
            "SELECT mentat._txfn_err($1)",
            &[pgrx::datum::DatumWithOid::from(sql)],
        )
        .expect("err helper")
        .unwrap_or_default()
    }

    /// Stores sa ("a", n 1) and sb ("b-secret", n 2), each with the :tf/*
    /// schema, and two roles: pgm_tf_a with read-write on sa only, and
    /// pgm_tf_ro with read-write on sa and read-only on sb.
    fn two_stores_two_roles() {
        named("sa");
        named("sb");
        t_on("sa", r#"[{:tf/name "a" :tf/n 1}]"#);
        t_on("sb", r#"[{:tf/name "b-secret" :tf/n 2}]"#);
        Spi::run(
            "DO $$ BEGIN
               IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'pgm_tf_a') THEN
                 CREATE ROLE pgm_tf_a NOSUPERUSER;
               END IF;
               IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'pgm_tf_ro') THEN
                 CREATE ROLE pgm_tf_ro NOSUPERUSER;
               END IF;
             END $$;
             REVOKE ALL ON SCHEMA mentat_sb FROM PUBLIC;
             REVOKE ALL ON ALL TABLES IN SCHEMA mentat_sb FROM PUBLIC;
             GRANT USAGE ON SCHEMA mentat TO pgm_tf_a, pgm_tf_ro;
             GRANT SELECT ON mentat.stores, mentat.cache_generation TO pgm_tf_a, pgm_tf_ro;
             GRANT UPDATE ON mentat.cache_generation TO pgm_tf_a, pgm_tf_ro;
             GRANT USAGE ON SCHEMA mentat_sa TO pgm_tf_a, pgm_tf_ro;
             GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA mentat_sa
               TO pgm_tf_a, pgm_tf_ro;
             GRANT USAGE, UPDATE ON ALL SEQUENCES IN SCHEMA mentat_sa TO pgm_tf_a, pgm_tf_ro;
             GRANT USAGE ON SCHEMA mentat_sb TO pgm_tf_ro;
             GRANT SELECT ON ALL TABLES IN SCHEMA mentat_sb TO pgm_tf_ro;",
        )
        .expect("roles");
    }

    /// A tx-fn that reads sb's "n 2" name and asserts it into its own store.
    const READ_B: &str = "(fn [db] \
       (let [b (mentat.store/db (mentat.store/open \"sb\")) \
             v (ffirst (mentat.store/q b '[:find ?v :where [?e :tf/n 2] [?e :tf/name ?v]]))] \
         [{:tf/name (str \"leak-\" v) :tf/n 3}]))";

    /// A tx-fn reaches only the store it runs on (issue #389 review). The
    /// author of a :db/fn on sa, with no rights on sb, wrote fns that read and
    /// destroy sb's data through an invoker who has rights on sb. Both fail;
    /// edn_eval, where the caller runs their own code, still opens both.
    #[pg_test]
    fn txfn_cannot_reach_another_store_through_its_invoker() {
        setup();
        two_stores_two_roles();
        Spi::run("SET LOCAL ROLE pgm_tf_a").expect("set role");
        install_on("sa", ":x/readb", READ_B);
        install_on(
            "sa",
            ":x/wipeb",
            "(fn [db] \
               (mentat.store/transact (mentat.store/open \"sb\") \
                 [[:db/retractEntity [:tf/name \"b-secret\"]]]) \
               [{:tf/name \"harmless\" :tf/n 4}])",
        );
        // The author can't read sb.
        let e = err_sql("SELECT edn_eval('(mentat.store/open \"sb\")')");
        assert!(e.contains("permission denied for schema mentat_sb"), "{e}");
        Spi::run("RESET ROLE").expect("reset role");

        Spi::run("SET LOCAL ROLE pgm_tf_ro").expect("set role");
        // The invoker can read both stores from edn_eval, in one script.
        let both: String = Spi::get_one(
            "SELECT edn_eval($$(let [q '[:find [?v ...] :where [_ :tf/name ?v]] \
                                    a (mentat.store/db (mentat.store/open \"sa\")) \
                                    b (mentat.store/db (mentat.store/open \"sb\"))] \
                                [(mentat.store/q a q) (mentat.store/q b q)])$$)",
        )
        .expect("edn_eval")
        .expect("NULL");
        assert_eq!(both, r#"[["a"] ["b-secret"]]"#);
        // The tx-fn can't, when the same role invokes it.
        let e = err_on("sa", "[[:x/readb]]");
        assert!(
            e.contains(":db.error/txfn-store-scope")
                && e.contains("only open the store it runs on ('sa')")
                && e.contains("'sb' is out of its scope"),
            "got: {e}"
        );
        Spi::run("RESET ROLE").expect("reset role");
        assert_eq!(n_on("sa", "leak-b-secret"), None, "nothing copied from sb");

        // A role with every right (this superuser) gets the same refusal.
        let e = err_on("sa", "[[:x/wipeb]]");
        assert!(e.contains(":db.error/txfn-store-scope"), "got: {e}");
        assert_eq!(n_on("sb", "b-secret"), Some(2), "sb is unchanged");
        assert_eq!(n_on("sa", "harmless"), None);
    }

    /// The default store's tx-fns are scoped the same way, and a tx-fn can't
    /// open the default store from a named one, or learn whether a name is a
    /// store: every other name gets the same error.
    #[pg_test]
    fn txfn_store_scope_covers_the_default_store_and_unknown_names() {
        setup();
        named("sa");
        t_on("sa", r#"[{:tf/name "a" :tf/n 1}]"#);
        t(r#"[{:tf/name "d" :tf/n 1}]"#);
        let opener = |name: &str| {
            format!(
                "(fn [db] (mentat.store/q (mentat.store/db (mentat.store/open {name:?})) \
                   '[:find ?v . :where [_ :tf/name ?v]]) [])"
            )
        };
        for (i, (store, other)) in [
            ("default", "sa"),
            ("default", "nope"),
            ("sa", "default"),
            ("sa", "sb"),
            ("sa", "nope"),
            ("sa", "SA"),
        ]
        .into_iter()
        .enumerate()
        {
            // One ident per fn: on pg a second {:db/ident X :db/fn ..} moves X to
            // a new entid, so it no longer names the fn.
            let ident = format!(":x/open{i}");
            install_on(store, &ident, &opener(other));
            let e = err_on(store, &format!("[[{ident}]]"));
            assert!(
                e.contains(&format!(
                    ":db.error/txfn-store-scope a transaction function can only open \
                     the store it runs on ('{store}'); '{other}' is out of its scope"
                )),
                "{store} -> {other}: {e}"
            );
        }
        assert_eq!(n_of("d"), Some(1));
        assert_eq!(n_on("sa", "a"), Some(1));
    }

    /// Inside a tx-fn every read reaches its own store: open, open by its
    /// name, q, entity, pull, read, entities and as-of. Handles no open
    /// returned are refused, and a tx-fn can't transact or call with: it
    /// returns tx-data instead (issue #405: a tx-fn transacting its own store
    /// recursed to the stack depth limit).
    #[pg_test]
    fn txfn_reads_its_own_store_and_cannot_transact() {
        setup();
        named("sa");
        t_on("sa", r#"[{:tf/name "a" :tf/n 1}]"#);
        t_on("sa", r#"[{:tf/name "b" :tf/n 1}]"#);
        install_on(
            "sa",
            ":x/self",
            "(fn [db] \
               (let [c (mentat.store/open) \
                     c2 (mentat.store/open \"sa\") \
                     e (mentat.store/q db '[:find ?e . :where [?e :tf/name \"a\"]]) \
                     t (mentat.store/q db '[:find ?tx . :where [_ :tf/name \"a\" ?tx]]) \
                     n (+ (:tf/n (mentat.store/entity db e)) \
                          (:tf/n (mentat.store/pull (mentat.store/db c2) e [:tf/n])) \
                          (mentat.store/read db [:tf/name \"b\"] :tf/n) \
                          (count (mentat.store/entities (mentat.store/db c) :tf/name)) \
                          (count (mentat.store/q (mentat.store/as-of db t) \
                                   '[:find ?v :where [_ :tf/name ?v]])))] \
                 [{:tf/name \"self\" :tf/n (if (= 1 c c2) n -1)}]))",
        );
        t_on("sa", "[[:x/self]]");
        // 1 + 1 + 1 + 2 names now + 1 name as of "a"'s tx.
        assert_eq!(n_on("sa", "self"), Some(6));
        assert_eq!(n_of("self"), None);

        for (ident, body, want) in [
            (
                ":x/tx",
                "(mentat.store/transact (mentat.store/open) [[:x/tx]])",
                ":db.error/txfn-transact",
            ),
            (
                ":x/with",
                "(mentat.store/with db [{:tf/name \"w\" :tf/n 1}])",
                ":db.error/txfn-transact",
            ),
            (
                ":x/h2",
                "(mentat.store/q (mentat.store/db 2) '[:find ?v . :where [_ :tf/name ?v]])",
                "no open store for handle 2",
            ),
            (
                ":x/map",
                "(mentat.store/q {:mentat.store/db true :mentat.store/conn 2} \
                   '[:find ?v . :where [_ :tf/name ?v]])",
                "no open store for handle 2",
            ),
        ] {
            install_on("sa", ident, &format!("(fn [db] {body} [])"));
            let e = err_on("sa", &format!("[[{ident}] {{:tf/name \"x\" :tf/n 1}}]"));
            assert!(e.contains(want), "{ident}: {e}");
        }
        assert_eq!(n_on("sa", "x"), None);
        assert_eq!(n_on("sa", "w"), None);
    }
}
