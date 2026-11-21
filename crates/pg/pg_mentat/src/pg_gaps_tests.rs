// Tests for the optional pg_gaps integration:
//   helpers:   mentat.has_pg_gaps(), mentat.gaps_column(attr),
//              mentat.create_gaps_view(view, attrs, store, entities),
//              mentat.gaps_declare(model, view, attr, domain, stripe)
//   where-fns: [(gaps-marginals "model" :attr) [[?e ?value ?p]]]
//              [(gaps-impute "model" :attr) [[?e ?value ?p]]]
//
// pg_gaps is OPTIONAL (PostgreSQL 17+). The view, its errors and the where-fn
// argument checks run everywhere; the inference tests run only when pg_gaps
// is installed in the test cluster and skip otherwise.

#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod tests {
    use pgrx::prelude::*;
    use pgrx::JsonB;

    /// Patients 1..=60 with :gx/name; :gx/smoker (an enum ref) is missing for
    /// every 7th, :gx/cancer (boolean) for every 5th. Smokers (every 3rd) have
    /// cancer when also even; nobody else does.
    fn setup() {
        crate::ensure_extension_loaded();
        Spi::run("SELECT bootstrap_schema()").expect("bootstrap_schema failed");
        Spi::run(
            "CREATE OR REPLACE FUNCTION mentat._gx_capture(stmt TEXT) RETURNS TEXT
             LANGUAGE plpgsql AS $$
             BEGIN
                 EXECUTE stmt;
                 RETURN '';
             EXCEPTION WHEN OTHERS THEN
                 RETURN SQLERRM;
             END;
             $$",
        )
        .expect("error-capture helper");
        Spi::run(
            "SELECT edn_t('[
                {:db/ident :gx/name   :db/valueType :db.type/string  :db/cardinality :db.cardinality/one
                 :db/unique :db.unique/identity}
                {:db/ident :gx/smoker :db/valueType :db.type/ref     :db/cardinality :db.cardinality/one}
                {:db/ident :gx/cancer :db/valueType :db.type/boolean :db/cardinality :db.cardinality/one}
                {:db/ident :gx/level  :db/valueType :db.type/keyword :db/cardinality :db.cardinality/one}
                {:db/ident :gx/age    :db/valueType :db.type/long    :db/cardinality :db.cardinality/one}
                {:db/ident :gx/tags   :db/valueType :db.type/keyword :db/cardinality :db.cardinality/many}
                {:db/ident :gx/weight :db/valueType :db.type/double  :db/cardinality :db.cardinality/one}
                {:db/ident :yn/yes} {:db/ident :yn/no}
             ]'::TEXT)",
        )
        .expect("schema");
        Spi::run(
            "SELECT edn_t((SELECT '[' || string_agg(format('{:gx/name \"p%s\" %s %s %s}', i,
                 CASE WHEN i % 7 = 0 THEN '' WHEN i % 3 = 0 THEN ':gx/smoker :yn/yes'
                      ELSE ':gx/smoker :yn/no' END,
                 CASE WHEN i % 5 = 0 THEN '' WHEN i % 3 = 0 AND i % 2 = 0 THEN ':gx/cancer true'
                      ELSE ':gx/cancer false' END,
                 CASE WHEN i % 2 = 0 THEN ':gx/level :lvl/high :gx/age 40' ELSE ':gx/age 30' END),
               ' ') || ']' FROM generate_series(1, 60) i))",
        )
        .expect("data");
    }

    fn capture_error(sql: &str) -> String {
        let escaped = sql.replace('\'', "''");
        Spi::get_one::<String>(&format!("SELECT mentat._gx_capture('{}')", escaped))
            .expect("capture")
            .unwrap_or_default()
    }

    fn installed() -> bool {
        // Try to install it in this database; the tests skip if it isn't built.
        let _ = capture_error("CREATE EXTENSION IF NOT EXISTS pg_gaps");
        Spi::get_one::<bool>("SELECT mentat.has_pg_gaps()")
            .ok()
            .flatten()
            .unwrap_or(false)
    }

    /// security_invoker views, which create_gaps_view makes, are PostgreSQL 15+.
    fn views_supported() -> bool {
        Spi::get_one::<i32>("SELECT current_setting('server_version_num')::int")
            .expect("version")
            .expect("NULL")
            >= 150000
    }

    fn q(query: &str) -> serde_json::Value {
        let escaped = query.replace('\'', "''");
        Spi::get_one::<JsonB>(&format!("SELECT edn_q('{escaped}', '{{}}'::jsonb)"))
            .expect("edn_q")
            .expect("NULL")
            .0
    }

    fn count(sql: &str) -> i64 {
        Spi::get_one::<i64>(sql).expect(sql).expect("NULL")
    }

    // ---- the view (no pg_gaps needed) --------------------------------------

    #[pg_test]
    fn gaps_column_names() {
        setup();
        for (ident, col) in [
            (":patient/smoker", "patient_smoker"),
            (":Patient/Is-Smoker?", "patient_is_smoker_"),
            (":a.b/c", "a_b_c"),
        ] {
            let got = Spi::get_one::<String>(&format!("SELECT mentat.gaps_column('{ident}')"))
                .expect("gaps_column")
                .expect("NULL");
            assert_eq!(got, col, "{ident}");
        }
    }

    /// One row per entity of the population, each value in pg_gaps' spelling,
    /// NULL where the entity has no current value; the view follows writes.
    #[pg_test]
    fn gaps_create_gaps_view_shapes_values() {
        setup();
        if !views_supported() {
            return; // create_gaps_view_refuses_old_servers covers it
        }
        let v = Spi::get_one::<String>(
            "SELECT mentat.create_gaps_view('public.gx_pt', \
             ARRAY[':gx/smoker', ':gx/cancer', ':gx/level', ':gx/age', ':gx/name'], \
             'default', ':gx/name')",
        )
        .expect("create")
        .expect("NULL");
        assert_eq!(v, "public.gx_pt");
        assert_eq!(count("SELECT count(*) FROM public.gx_pt"), 60);
        assert_eq!(
            count("SELECT count(*) FROM public.gx_pt WHERE gx_smoker IS NULL"),
            8
        );
        assert_eq!(
            count("SELECT count(*) FROM public.gx_pt WHERE gx_cancer IS NULL"),
            12
        );
        assert_eq!(
            count("SELECT count(*) FROM public.gx_pt WHERE gx_level IS NULL"),
            30
        );
        let row = Spi::get_one::<String>(
            "SELECT concat_ws('|', gx_smoker, gx_cancer, gx_level, gx_age, gx_name) \
             FROM public.gx_pt WHERE gx_name = 'p6'",
        )
        .expect("row")
        .expect("NULL");
        assert_eq!(row, ":yn/yes|true|:lvl/high|40|p6");
        let comment = Spi::get_one::<String>("SELECT col_description('public.gx_pt'::regclass, 2)")
            .expect("comment")
            .expect("NULL");
        assert_eq!(comment, ":gx/smoker");
        // A write is seen at once: the view reads the current state.
        Spi::run("SELECT edn_t('[{:gx/name \"p7\" :gx/smoker :yn/yes}]')").expect("write");
        assert_eq!(
            count("SELECT count(*) FROM public.gx_pt WHERE gx_smoker IS NULL"),
            7
        );
        // Without a population attribute: entities with any listed attribute.
        Spi::get_one::<String>(
            "SELECT mentat.create_gaps_view('public.gx_lvl', ARRAY[':gx/level'])",
        )
        .expect("create");
        assert_eq!(count("SELECT count(*) FROM public.gx_lvl"), 30);
    }

    #[pg_test]
    fn gaps_create_gaps_view_named_store() {
        setup();
        if !views_supported() {
            return; // create_gaps_view_refuses_old_servers covers it
        }
        Spi::run("SELECT create_store('gxs')").expect("store");
        Spi::run(
            "SELECT t('gxs', '[{:db/ident :s/k :db/valueType :db.type/keyword \
             :db/cardinality :db.cardinality/one}]')",
        )
        .expect("schema");
        Spi::run("SELECT t('gxs', '[{:s/k :v/a} {:s/k :v/b} {:s/k :v/a}]')").expect("data");
        Spi::get_one::<String>(
            "SELECT mentat.create_gaps_view('public.gx_store', ARRAY[':s/k'], 'gxs')",
        )
        .expect("create");
        assert_eq!(
            count("SELECT count(*) FROM public.gx_store WHERE s_k = ':v/a'"),
            2
        );
        // The default store has no :s/k: it isn't read from there.
        let err = capture_error("SELECT mentat.create_gaps_view('public.gx_x', ARRAY[':s/k'])");
        assert!(err.contains("attribute-not-found"), "got: {err}");
        Spi::run("SELECT drop_store('gxs')").expect("drop");
    }

    #[pg_test]
    fn gaps_create_gaps_view_refuses_bad_input() {
        setup();
        if !views_supported() {
            return;
        }
        for (sql, want) in [
            (
                "SELECT mentat.create_gaps_view('x', ARRAY[':gx/tags'])",
                "cardinality many",
            ),
            (
                "SELECT mentat.create_gaps_view('x', ARRAY[':gx/weight'])",
                ":db.type/double",
            ),
            (
                "SELECT mentat.create_gaps_view('x', ARRAY[':gx/nope'])",
                "attribute-not-found",
            ),
            (
                "SELECT mentat.create_gaps_view('x', ARRAY[':gx/smoker'], 'nostore')",
                "store-not-found",
            ),
            (
                "SELECT mentat.create_gaps_view('bad name', ARRAY[':gx/smoker'])",
                "Invalid view name",
            ),
            (
                "SELECT mentat.create_gaps_view('x', ARRAY[]::text[])",
                "at least one attribute",
            ),
            (
                "SELECT mentat.create_gaps_view('x', ARRAY[':gx/smoker', ':gx/smoker'])",
                "already uses",
            ),
            (
                "SELECT mentat.create_gaps_view('x', ARRAY[':gx/smoker'], 'default', ':gx/none')",
                "entities argument",
            ),
        ] {
            let err = capture_error(sql);
            assert!(err.contains(want), "{sql}: {err}");
        }
        // A column name over PostgreSQL's 63 bytes would be truncated: refused.
        let long = format!(":gx/{}", "long".repeat(20));
        Spi::run(&format!(
            "SELECT edn_t('[{{:db/ident {long} :db/valueType :db.type/long \
             :db/cardinality :db.cardinality/one}}]')"
        ))
        .expect("long ident");
        let err = capture_error(&format!(
            "SELECT mentat.create_gaps_view('x', ARRAY['{long}'])"
        ));
        assert!(err.contains("63-byte"), "got: {err}");
    }

    /// The view is security_invoker: a role that can't read the store's
    /// tables can't read them through the view either.
    #[pg_test]
    fn gaps_create_gaps_view_runs_as_the_caller() {
        setup();
        if !views_supported() {
            return; // create_gaps_view_refuses_old_servers covers it
        }
        Spi::get_one::<String>(
            "SELECT mentat.create_gaps_view('public.gx_sec', ARRAY[':gx/smoker'])",
        )
        .expect("create");
        Spi::run("DROP ROLE IF EXISTS gx_reader").expect("drop role");
        Spi::run("CREATE ROLE gx_reader").expect("role");
        Spi::run("GRANT USAGE ON SCHEMA public TO gx_reader").expect("grant");
        Spi::run("GRANT SELECT ON public.gx_sec TO gx_reader").expect("grant");
        let err = capture_error("SET LOCAL ROLE gx_reader; SELECT count(*) FROM public.gx_sec");
        Spi::run("RESET ROLE").expect("reset");
        assert!(err.contains("permission denied"), "got: {err}");
    }

    #[pg_test]
    fn gaps_where_fns_reject_bad_arity_and_args() {
        setup();
        for (query, want) in [
            (
                "[:find ?e :where [(gaps-marginals \"m\") [[?e ?v ?p]]]]",
                "fn-arity",
            ),
            (
                "[:find ?e :where [(gaps-impute \"m\" :gx/smoker 3) [[?e ?v ?p]]]]",
                "fn-arity",
            ),
            (
                "[:find ?e :where [(gaps-marginals :m :gx/smoker) [[?e ?v ?p]]]]",
                "model name",
            ),
            (
                "[:find ?e :where [(gaps-marginals \"m\" \"x\") [[?e ?v ?p]]]]",
                "keyword attribute",
            ),
            (
                "[:find ?e :where [(gaps-marginals \"m\" :gx/smoker) ?e]]",
                "relation binding",
            ),
        ] {
            let err = capture_error(&format!("SELECT edn_q('{query}', '{{}}'::jsonb)"));
            assert!(err.contains(want), "{query}: {err}");
        }
    }

    #[pg_test]
    fn gaps_create_gaps_view_refuses_old_servers() {
        setup();
        if views_supported() {
            return;
        }
        let err = capture_error("SELECT mentat.create_gaps_view('x', ARRAY[':gx/smoker'])");
        assert!(err.contains("PostgreSQL 15"), "got: {err}");
    }

    // ---- with pg_gaps --------------------------------------------------------

    #[pg_test]
    fn gaps_where_fns_need_pg_gaps() {
        setup();
        if installed() {
            return;
        }
        let err = capture_error(
            "SELECT edn_q('[:find ?e :where [(gaps-marginals \"m\" :gx/smoker) [[?e ?v ?p]]]]', \
             '{}'::jsonb)",
        );
        assert!(
            err.contains("missing-extension") && err.contains("pg_gaps"),
            "got: {err}"
        );
    }

    #[pg_test]
    fn gaps_declare_errors_clearly_when_absent() {
        setup();
        if !views_supported() {
            return; // create_gaps_view_refuses_old_servers covers it
        }
        if installed() {
            return; // covered by the inference test below
        }
        Spi::get_one::<String>(
            "SELECT mentat.create_gaps_view('public.gx_abs', ARRAY[':gx/smoker'])",
        )
        .expect("create");
        let err = capture_error("SELECT mentat.gaps_declare('m', 'public.gx_abs', ':gx/smoker')");
        assert!(
            err.contains("missing-extension") && err.contains("pg_gaps"),
            "got: {err}"
        );
    }

    /// The whole loop: view, model declared from mentat idents, CPTs learned
    /// from the data, and the posteriors read back in Datalog joined to the
    /// entities' other attributes.
    #[pg_test]
    fn gaps_marginals_and_impute_from_datalog() {
        setup();
        if !installed() || !views_supported() {
            return;
        }
        Spi::get_one::<String>(
            "SELECT mentat.create_gaps_view('public.gx_m', ARRAY[':gx/smoker', ':gx/cancer'], \
             'default', ':gx/name')",
        )
        .expect("create");
        Spi::run("SELECT gaps.create_model('gx_cancer')").expect("model");
        let dom = Spi::get_one::<Vec<String>>(
            "SELECT mentat.gaps_declare('gx_cancer', 'public.gx_m', ':gx/smoker')",
        )
        .expect("declare")
        .expect("NULL");
        assert_eq!(dom, vec![":yn/no".to_string(), ":yn/yes".to_string()]);
        Spi::run("SELECT mentat.gaps_declare('gx_cancer', 'public.gx_m', ':gx/cancer')")
            .expect("declare");
        Spi::run("SELECT gaps.add_factor('gx_cancer', 'f_s', 'gx_smoker')").expect("factor");
        Spi::run("SELECT gaps.add_factor('gx_cancer', 'f_c', 'gx_cancer', ARRAY['gx_smoker'])")
            .expect("factor");
        Spi::run("SELECT count(*) FROM gaps.learn_cpts('gx_cancer')").expect("learn");
        let bad = count("SELECT count(*) FROM gaps.validate('gx_cancer') WHERE NOT ok");
        assert_eq!(bad, 0);

        // Marginals: every missing smoker value, both domain values, summing to 1.
        let r = q("[:find ?name ?v ?p \
                    :where [(gaps-marginals \"gx_cancer\" :gx/smoker) [[?e ?v ?p]]] \
                           [?e :gx/name ?name]]");
        let rows = r["result"].as_array().expect("rows");
        assert_eq!(rows.len(), 16, "{r}");
        let mut by_name: std::collections::BTreeMap<String, f64> = Default::default();
        for row in rows {
            let name = row[0].as_str().expect("name").to_string();
            let v = row[1].as_str().expect("value");
            assert!(v == ":yn/yes" || v == ":yn/no", "{row}");
            *by_name.entry(name).or_default() += row[2].as_f64().expect("p");
        }
        assert_eq!(by_name.len(), 8);
        for (name, total) in &by_name {
            assert!((total - 1.0).abs() < 1e-9, "{name}: {total}");
        }
        // p35 is missing smoker but has cancer false; p14 has both missing.
        assert!(
            by_name.contains_key("p35") && by_name.contains_key("p14"),
            "{by_name:?}"
        );

        // A value that is present is not a random variable: p6 gets no rows.
        let r = q("[:find ?v :where [?e :gx/name \"p6\"] \
                    [(gaps-marginals \"gx_cancer\" :gx/smoker) [[?e ?v ?p]]]]");
        assert_eq!(r["result"].as_array().map(|a| a.len()), Some(0), "{r}");

        // Impute: one completion per missing value, from the domain. A boolean
        // attribute's value comes back as a JSON boolean, as edn_q decodes
        // "true" / "false".
        let r = q("[:find ?e ?v ?p :where [(gaps-impute \"gx_cancer\" :gx/cancer) [[?e ?v ?p]]]]");
        let rows = r["result"].as_array().expect("rows");
        assert_eq!(rows.len(), 12, "{r}");
        for row in rows {
            assert!(row[1].is_boolean(), "{row}");
            let p = row[2].as_f64().expect("p");
            assert!(p > 0.0 && p <= 1.0, "{row}");
        }

        // The model reads current data: give p14 a smoker value and its
        // smoker marginal is gone.
        Spi::run("SELECT edn_t('[{:gx/name \"p14\" :gx/smoker :yn/no}]')").expect("write");
        let r = q("[:find ?v :where [?e :gx/name \"p14\"] \
                    [(gaps-marginals \"gx_cancer\" :gx/smoker) [[?e ?v ?p]]]]");
        assert_eq!(r["result"].as_array().map(|a| a.len()), Some(0), "{r}");

        // ?component is bound; one entity's rows share it.
        let r = q("[:find ?e ?v ?c :where [?e :gx/name \"p35\"] \
                    [(gaps-marginals \"gx_cancer\" :gx/smoker) [[?e ?v ?p ?c]]]]");
        let rows = r["result"].as_array().expect("rows");
        assert_eq!(rows.len(), 2, "{r}");
        assert_eq!(rows[0][2], rows[1][2], "{r}");

        // Inside an or branch.
        let r = q(
            "[:find ?e ?v :where (or [(gaps-impute \"gx_cancer\" :gx/smoker) [[?e ?v ?p]]] \
                                           [(gaps-impute \"gx_cancer\" :gx/cancer) [[?e ?v ?p]]])]",
        );
        assert_eq!(r["result"].as_array().map(|a| a.len()), Some(7 + 12), "{r}");

        Spi::run("SELECT gaps.drop_model('gx_cancer')").expect("drop");
    }

    /// Two views in one model with the same column name: the where-fn reads
    /// only the column whose comment is the asked-for ident.
    #[pg_test]
    fn gaps_where_fn_matches_the_ident_not_the_column_name() {
        setup();
        if !installed() || !views_supported() {
            return;
        }
        Spi::get_one::<String>(
            "SELECT mentat.create_gaps_view('public.gx_a', ARRAY[':gx/smoker'], 'default', ':gx/name')",
        )
        .expect("create");
        // A plain table with a same-named column, declared in the same model.
        Spi::run(
            "CREATE TABLE public.gx_other (e bigint PRIMARY KEY, gx_smoker text); \
             INSERT INTO public.gx_other SELECT i, CASE WHEN i % 2 = 0 THEN ':yn/no' END \
             FROM generate_series(1, 10) i",
        )
        .expect("table");
        Spi::run("SELECT gaps.create_model('gx_two')").expect("model");
        Spi::run("SELECT mentat.gaps_declare('gx_two', 'public.gx_a', ':gx/smoker')")
            .expect("declare");
        Spi::run(
            "SELECT gaps.declare_attribute('gx_two', 'public.gx_other', 'gx_smoker', \
             ARRAY[':yn/no', ':yn/yes']); \
             SELECT gaps.add_stripe('gx_two', 'other', 'public.gx_other', 'gx_smoker'); \
             SELECT gaps.add_factor('gx_two', 'f_a', 'gx_smoker'); \
             SELECT gaps.add_factor('gx_two', 'f_o', 'other'); \
             SELECT count(*) FROM gaps.learn_cpts('gx_two')",
        )
        .expect("model");
        assert_eq!(
            count("SELECT count(*) FROM gaps.marginals('gx_two') WHERE attname = 'gx_smoker'"),
            2 * (8 + 5)
        );
        let r = q("[:find ?e ?v :where [(gaps-marginals \"gx_two\" :gx/smoker) [[?e ?v ?p]]]]");
        assert_eq!(r["result"].as_array().map(|a| a.len()), Some(2 * 8), "{r}");
        Spi::run("SELECT gaps.drop_model('gx_two')").expect("drop");
    }

    /// A model over a named store's view, queried in that store.
    #[pg_test]
    fn gaps_where_fn_named_store() {
        setup();
        if !installed() || !views_supported() {
            return;
        }
        Spi::run("SELECT create_store('gxn')").expect("store");
        Spi::run(
            "SELECT t('gxn', '[{:db/ident :n/k :db/valueType :db.type/keyword \
             :db/cardinality :db.cardinality/one} {:db/ident :n/id :db/valueType :db.type/long \
             :db/cardinality :db.cardinality/one}]')",
        )
        .expect("schema");
        Spi::run(
            "SELECT t('gxn', (SELECT '[' || string_agg(format('{:n/id %s %s}', i, \
               CASE WHEN i % 4 = 0 THEN '' WHEN i % 2 = 0 THEN ':n/k :v/a' ELSE ':n/k :v/b' END), ' ') \
               || ']' FROM generate_series(1, 20) i))",
        )
        .expect("data");
        Spi::get_one::<String>(
            "SELECT mentat.create_gaps_view('public.gx_n', ARRAY[':n/k'], 'gxn', ':n/id')",
        )
        .expect("create");
        Spi::run("SELECT gaps.create_model('gx_n')").expect("model");
        Spi::run("SELECT mentat.gaps_declare('gx_n', 'public.gx_n', ':n/k')").expect("declare");
        Spi::run("SELECT gaps.add_factor('gx_n', 'f', 'n_k')").expect("factor");
        Spi::run("SELECT count(*) FROM gaps.learn_cpts('gx_n')").expect("learn");
        let out = Spi::get_one::<JsonB>(
            "SELECT q('gxn', '[:find ?id ?v ?p :where [(gaps-impute \"gx_n\" :n/k) [[?e ?v ?p]]] \
             [?e :n/id ?id]]', '{}'::jsonb)",
        )
        .expect("q")
        .expect("NULL")
        .0;
        let mut ids: Vec<i64> = out["result"]
            .as_array()
            .expect("rows")
            .iter()
            .map(|r| r[0].as_i64().expect("id"))
            .collect();
        ids.sort();
        assert_eq!(ids, vec![4, 8, 12, 16, 20], "{out}");
        Spi::run("SELECT gaps.drop_model('gx_n')").expect("drop");
        Spi::run("SELECT drop_store('gxn')").expect("drop");
    }

    #[pg_test]
    fn gaps_declare_explicit_domain_and_errors() {
        setup();
        if !installed() || !views_supported() {
            return;
        }
        Spi::get_one::<String>(
            "SELECT mentat.create_gaps_view('public.gx_d', ARRAY[':gx/smoker', ':gx/level'])",
        )
        .expect("create");
        Spi::run("SELECT gaps.create_model('gx_dom')").expect("model");
        let dom = Spi::get_one::<Vec<String>>(
            "SELECT mentat.gaps_declare('gx_dom', 'public.gx_d', ':gx/level', \
             ARRAY[':lvl/low', ':lvl/high'], 'lvl')",
        )
        .expect("declare")
        .expect("NULL");
        assert_eq!(dom, vec![":lvl/low".to_string(), ":lvl/high".to_string()]);
        assert_eq!(
            count(
                "SELECT count(*) FROM gaps.stripe s JOIN gaps.model m USING (model_id) \
                   WHERE m.name = 'gx_dom' AND s.name = 'lvl'"
            ),
            1
        );
        let err = capture_error("SELECT mentat.gaps_declare('gx_dom', 'public.gx_d', ':gx/name')");
        assert!(err.contains("has no column for"), "got: {err}");
        Spi::run("SELECT gaps.drop_model('gx_dom')").expect("drop");
    }
}
