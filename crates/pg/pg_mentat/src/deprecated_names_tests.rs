// 1.9.0 rename: edn_t/edn_q/edn_pull/edn_eval are the SQL names; the pre-1.9.0
// mentat_transact/mentat_query/mentat_pull/mentat_eval are deprecated SQL
// wrappers, and mentat.t/q/pull alias the new names.

#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod tests {
    use pgrx::prelude::*;

    fn text(sql: &str) -> String {
        Spi::get_one::<String>(sql).expect(sql).expect("NULL")
    }

    /// Define :dn/name and assert one entity through `tx_fn`; return its eid.
    fn seed(tx_fn: &str) -> i64 {
        crate::ensure_extension_loaded();
        Spi::run("SELECT bootstrap_schema()").expect("bootstrap_schema");
        text(&format!(
            "SELECT {tx_fn}('[{{:db/ident :dn/name :db/valueType :db.type/string \
               :db/cardinality :db.cardinality/one}}]')"
        ));
        let r = text(&format!(
            "SELECT {tx_fn}('[{{:db/id \"a\" :dn/name \"Alice\"}}]')"
        ));
        let j: serde_json::Value = serde_json::from_str(&r).expect("tx report json");
        j["tempids"]["a"].as_i64().expect("tempid a")
    }

    const Q: &str = "'[:find ?n :where [?e :dn/name ?n]]'";

    #[pg_test]
    fn deprecated_wrappers_match_new_names() {
        let eid = seed("mentat_transact"); // old name still transacts
        for (old, new) in [
            (
                format!("mentat_query({Q}, '{{}}')"),
                format!("edn_q({Q}, '{{}}')"),
            ),
            (format!("mentat_query({Q})"), format!("edn_q({Q}, '{{}}')")),
            (
                format!("mentat_pull('[:dn/name]', {eid})"),
                format!("edn_pull('[:dn/name]', {eid})"),
            ),
        ] {
            let o = text(&format!("SELECT ({old})::TEXT"));
            assert_eq!(o, text(&format!("SELECT ({new})::TEXT")), "{old}");
            assert!(o.contains("Alice"), "{old} -> {o}");
        }
        // The wrappers are marked deprecated.
        let c = text("SELECT obj_description('public.mentat_query(text,jsonb)'::regprocedure)");
        assert_eq!(c, "Deprecated since 1.9.0: use edn_q");
    }

    #[pg_test]
    fn short_aliases_call_new_names() {
        let eid = seed("mentat.t");
        assert_eq!(
            text(&format!("SELECT mentat.q({Q})::TEXT")),
            text(&format!("SELECT edn_q({Q}, '{{}}')::TEXT"))
        );
        assert_eq!(
            text(&format!("SELECT mentat.pull('[:dn/name]', {eid})::TEXT")),
            text(&format!("SELECT edn_pull('[:dn/name]', {eid})::TEXT"))
        );
        let body =
            text("SELECT prosrc FROM pg_proc WHERE oid = 'mentat.q(text,jsonb)'::regprocedure");
        assert!(body.contains("edn_q"), "mentat.q body: {body}");
    }

    #[cfg(feature = "script")]
    #[pg_test]
    fn deprecated_mentat_eval_matches_edn_eval() {
        crate::ensure_extension_loaded();
        assert_eq!(text("SELECT mentat_eval('(+ 1 2)')"), "3");
        assert_eq!(text("SELECT edn_eval('(+ 1 2)')"), "3");
    }

    #[cfg(not(feature = "script"))]
    #[pg_test]
    fn no_eval_without_script_feature() {
        crate::ensure_extension_loaded();
        for f in ["edn_eval(text)", "mentat_eval(text)"] {
            let absent = Spi::get_one::<bool>(&format!("SELECT to_regprocedure('{f}') IS NULL"))
                .expect("spi")
                .expect("NULL");
            assert!(absent, "{f} must not exist in a non-script build");
        }
    }
}
