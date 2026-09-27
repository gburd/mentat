// Automatic index management (1.10.0): mentat_tune_indexes, the
// mentat.managed_indexes registry, and the mentat.auto_index GUCs.

#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod tests {
    use pgrx::prelude::*;

    fn setup() {
        crate::ensure_extension_loaded();
        Spi::run("SELECT bootstrap_schema()").expect("bootstrap_schema failed");
        Spi::run(
            "SELECT edn_t('[{:db/ident :ai/n :db/valueType :db.type/long :db/cardinality :db.cardinality/one}]'::TEXT)",
        )
        .expect("schema");
        Spi::run(
            "SELECT edn_t('[' || string_agg(format('{:ai/n %s}', g), ' ') || ']') \
             FROM generate_series(1, 300) g",
        )
        .expect("data");
        // pg_tests roll back, so autovacuum's reltuples for the shared tables
        // are 0 (every scan looks like 1 row); give the planner real stats.
        Spi::run("ANALYZE mentat.datoms_long_new, mentat.current_long").expect("analyze");
        Spi::run("SET LOCAL mentat.auto_index_min_queries = 5").expect("guc");
        Spi::run("SET LOCAL mentat.auto_index_min_rows = 100").expect("guc");
    }

    fn max_tx() -> i64 {
        Spi::get_one::<i64>("SELECT max(tx) FROM mentat.transactions")
            .expect("t")
            .expect("NULL")
    }

    /// n as-of range queries on :ai/n (temporal reads are the candidates).
    fn range_workload(n: usize) {
        let t = max_tx();
        for i in 0..n {
            Spi::run(&format!(
                "SELECT edn_q('[:find ?e :where [?e :ai/n ?n] [(>= ?n {})]]'::TEXT, \
                 '{{\"asOf\": {t}}}'::jsonb)",
                290 - i
            ))
            .expect("q");
        }
    }

    /// A write to the history table that does not scan the managed index.
    fn raw_write() {
        Spi::run(
            "INSERT INTO mentat.datoms_long_new (store_id, e, a, v, tx, added) \
             VALUES (0, 999999999, 1, 5, 1, false)",
        )
        .expect("write");
    }

    fn tune(dry_run: bool) -> Vec<(String, String)> {
        Spi::connect(|c| {
            let mut v = Vec::new();
            for r in c.select(
                &format!("SELECT action, index_name FROM mentat_tune_indexes({dry_run})"),
                None,
                &[],
            )? {
                v.push((
                    r.get::<String>(1)?.unwrap_or_default(),
                    r.get::<String>(2)?.unwrap_or_default(),
                ));
            }
            Ok::<_, pgrx::spi::SpiError>(v)
        })
        .expect("tune")
    }

    fn managed() -> i64 {
        Spi::get_one::<i64>("SELECT count(*) FROM mentat.managed_indexes")
            .expect("count")
            .unwrap_or(0)
    }

    fn index_exists(name: &str) -> bool {
        Spi::get_one::<bool>(&format!("SELECT to_regclass('mentat.{name}') IS NOT NULL"))
            .expect("regclass")
            .unwrap_or(false)
    }

    fn mentat_index_count() -> Option<i64> {
        Spi::get_one::<i64>("SELECT count(*) FROM pg_indexes WHERE schemaname = 'mentat'")
            .expect("count")
    }

    #[pg_test]
    fn test_auto_index_dry_run_changes_nothing_then_creates() {
        setup();
        range_workload(6);
        let planned = tune(true);
        assert_eq!(planned.len(), 1, "{planned:?}");
        assert_eq!(planned[0].0, "create");
        let idx = planned[0].1.clone();
        assert!(idx.starts_with("mentat_auto_datoms_long_new_a"), "{idx}");
        assert!(!index_exists(&idx), "dry run created {idx}");
        assert_eq!(managed(), 0, "dry run wrote the registry");

        let done = tune(false);
        assert_eq!(done, planned);
        assert!(index_exists(&idx));
        assert_eq!(managed(), 1);
        Spi::run("ANALYZE mentat.datoms_long_new").expect("analyze");
        let def =
            Spi::get_one::<String>(&format!("SELECT pg_get_indexdef('mentat.{idx}'::regclass)"))
                .expect("def")
                .expect("NULL");
        assert!(def.contains("(store_id, v, e, tx) WHERE ((a = "), "{def}");
        // Evidence was consumed: a second run has nothing to create.
        assert!(tune(false).iter().all(|(a, _)| a != "create"));
        // ... and the temporal range query now plans on it.
        let plan = Spi::get_one::<String>(&format!(
            "SELECT mentat_explain('[:find ?e :where [?e :ai/n ?n] [(>= ?n 295)]]', \
             '{{\"asOf\": {}}}'::jsonb)->>'explain_plan'",
            max_tx()
        ))
        .expect("explain")
        .expect("NULL");
        assert!(plan.contains(&idx), "{plan}");
    }

    #[pg_test]
    fn test_auto_index_below_threshold_creates_nothing() {
        setup();
        range_workload(4);
        assert!(tune(false).is_empty());
        Spi::run("SET LOCAL mentat.auto_index_min_rows = 100000").expect("guc");
        range_workload(6);
        assert!(tune(false).is_empty(), "table below auto_index_min_rows");
        // Current-state range queries are served by AVET: never evidence.
        Spi::run("SET LOCAL mentat.auto_index_min_rows = 100").expect("guc");
        Spi::run("DELETE FROM mentat.index_evidence").expect("reset");
        for i in 0..6 {
            Spi::run(&format!(
                "SELECT edn_q('[:find ?e :where [?e :ai/n ?n] [(< ?n {})]]'::TEXT, '{{}}'::jsonb)",
                10 + i
            ))
            .expect("q");
        }
        assert!(tune(false).is_empty());
    }

    #[pg_test]
    fn test_auto_index_drops_idle_index_after_window() {
        setup();
        range_workload(6);
        let idx = tune(false)[0].1.clone();
        // Window 0: an unscanned index on a table that took writes goes.
        Spi::run("SET LOCAL mentat.auto_index_idle_window = 0").expect("guc");
        raw_write();
        let r = tune(false);
        assert_eq!(r, vec![("drop".to_string(), idx.clone())]);
        assert!(!index_exists(&idx));
        assert_eq!(managed(), 0);
    }

    #[pg_test]
    fn test_auto_index_hysteresis_and_use_keep_index() {
        setup();
        range_workload(6);
        let idx = tune(false)[0].1.clone();
        Spi::run("ANALYZE mentat.datoms_long_new").expect("analyze");
        // Window 0 but the table took no writes: kept.
        Spi::run("SET LOCAL mentat.auto_index_idle_window = 0").expect("guc");
        assert!(tune(false).is_empty());
        // Writes, but the default 7d window: too young to drop (hysteresis).
        raw_write();
        Spi::run("RESET mentat.auto_index_idle_window").expect("guc");
        assert!(tune(false).is_empty());
        assert!(index_exists(&idx));
        // Window 0 and writes, but the index was scanned since the last
        // check: in use, so kept (and its idle clock restarts).
        Spi::run("SET LOCAL mentat.auto_index_idle_window = 0").expect("guc");
        // A scan the partial predicate provably matches (literal attribute).
        let attr = Spi::get_one::<i64>(&format!(
            "SELECT attr FROM mentat.managed_indexes WHERE index_name = '{idx}'"
        ))
        .expect("attr")
        .expect("NULL");
        Spi::run("SET LOCAL enable_seqscan = off").expect("guc");
        Spi::run("SET LOCAL enable_bitmapscan = off").expect("guc");
        Spi::run(&format!(
            "SELECT count(*) FROM mentat.datoms_long_new \
             WHERE store_id = 0 AND a = {attr} AND added AND v >= 295"
        ))
        .expect("scan");
        let r = tune(false);
        assert!(r.is_empty(), "{r:?}");
        assert!(index_exists(&idx));
    }

    #[pg_test]
    fn test_auto_index_never_drops_unmanaged() {
        setup();
        Spi::run("SET LOCAL mentat.auto_index_idle_window = 0").expect("guc");
        Spi::run("CREATE INDEX mentat_auto_user_made ON mentat.datoms_long_new (v)").expect("user");
        raw_write();
        let before = mentat_index_count();
        assert!(tune(false).is_empty());
        assert_eq!(before, mentat_index_count());
        assert!(index_exists("mentat_auto_user_made"));
        // The registry refuses anything not named mentat_auto_*.
        Spi::run(
            "DO $$ BEGIN \
               INSERT INTO mentat.managed_indexes (store_id, index_name, table_name, attr, kind, reason) \
               VALUES (0, 'datoms_long_new_pkey', 'mentat.datoms_long_new', 1, 'range', 'x'); \
               RAISE EXCEPTION 'registry accepted a non-managed name'; \
             EXCEPTION WHEN check_violation THEN NULL; END $$",
        )
        .expect("registry refuses non-managed names");
    }

    #[pg_test]
    fn test_auto_index_off_does_nothing() {
        setup();
        Spi::run("SET LOCAL mentat.auto_index = off").expect("guc");
        range_workload(6);
        assert!(tune(false).is_empty());
        Spi::run("SET LOCAL mentat.auto_index = schema").expect("guc");
        // No evidence was collected while off.
        assert!(tune(false).is_empty());
        assert_eq!(managed(), 0);
    }

    #[pg_test]
    fn test_auto_index_adaptive_runs_from_edn_t() {
        setup();
        Spi::run("SET LOCAL mentat.auto_index_every_n_tx = 1").expect("guc");
        // schema mode: the edn_t tick only flushes evidence.
        range_workload(6);
        Spi::run("SELECT edn_t('[{:ai/n 1000}]'::TEXT)").expect("write");
        assert_eq!(managed(), 0);
        // adaptive: the next edn_t tunes.
        Spi::run("SET LOCAL mentat.auto_index = adaptive").expect("guc");
        Spi::run("SELECT edn_t('[{:ai/n 1001}]'::TEXT)").expect("write");
        let hist = Spi::get_one::<i64>(
            "SELECT count(*) FROM mentat.managed_indexes WHERE table_name = 'mentat.datoms_long_new'",
        )
        .expect("count");
        assert_eq!(
            hist,
            Some(1),
            "the edn_t tick should have created the index"
        );
    }
}
