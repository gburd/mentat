// Concurrent writers under READ COMMITTED (#376), with two real sessions
// (dblink). Each case: session A runs its transaction up to COMMIT, session B
// starts the competing one, the test waits until B blocks on a lock (or
// finishes), commits A, then reads the outcome. Without the transactor's
// advisory locks every case left a cardinality-one attribute with two live
// values (or a unique value on two entities), and B never waited.
//
// The sessions commit, so each case runs in its own named store, dropped at the
// end. Needs contrib's dblink; skipped (passes) if the server lacks it.
// test/concurrent_writes.sh runs the same races from psql, at every isolation
// level.

#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod tests {
    use pgrx::prelude::*;

    fn text(sql: &str) -> String {
        Spi::get_one::<String>(sql)
            .unwrap_or_else(|e| panic!("{sql}: {e}"))
            .unwrap_or_default()
    }

    struct Race {
        store: String,
    }

    /// Run `sql` in session `c` (autocommit, or inside its open transaction).
    fn on(c: &str, sql: &str) {
        Spi::run(&format!(
            "SELECT dblink_exec('{c}', '{}')",
            sql.replace('\'', "''")
        ))
        .unwrap_or_else(|e| panic!("{c}: {sql}: {e}"))
    }

    /// `SELECT <expr>` in session `c`, as text.
    fn get(c: &str, expr: &str) -> String {
        text(&format!(
            "SELECT r FROM dblink('{c}', '{}') AS t(r text)",
            format!("SELECT ({expr})::text").replace('\'', "''")
        ))
    }

    impl Race {
        /// Connect two sessions, with a fresh store `name`. None without dblink.
        fn new(name: &str) -> Option<Race> {
            crate::ensure_extension_loaded();
            let available: i64 =
                Spi::get_one("SELECT count(*) FROM pg_available_extensions WHERE name = 'dblink'")
                    .unwrap()
                    .unwrap();
            if available == 0 {
                return None;
            }
            Spi::run(
                "CREATE EXTENSION IF NOT EXISTS dblink; \
                 CREATE OR REPLACE FUNCTION pg_temp.capture(stmt TEXT) RETURNS TEXT \
                 LANGUAGE plpgsql AS $$ BEGIN EXECUTE stmt; RETURN ''; \
                 EXCEPTION WHEN OTHERS THEN RETURN SQLERRM; END $$",
            )
            .expect("dblink");
            let conn = text(
                "SELECT pg_catalog.format('dbname=%s port=%s host=%s', current_database(), \
                   current_setting('port'), \
                   split_part(current_setting('unix_socket_directories'), ',', 1))",
            );
            Spi::run(&format!(
                "SELECT dblink_connect('cw_a', '{conn}'), dblink_connect('cw_b', '{conn}')"
            ))
            .expect("connect");
            for c in ["cw_a", "cw_b"] {
                on(c, "SET search_path TO mentat, public");
                on(c, "SET default_transaction_isolation = 'read committed'");
            }
            let race = Race {
                store: name.to_string(),
            };
            race.drop_store();
            on(
                "cw_a",
                &format!("DO $d$ BEGIN PERFORM create_store('{name}'); END $d$"),
            );
            Some(race)
        }

        fn drop_store(&self) {
            on(
                "cw_a",
                &format!(
                    "DO $d$ BEGIN PERFORM drop_store('{}'); \
                     EXCEPTION WHEN OTHERS THEN NULL; END $d$",
                    self.store
                ),
            );
        }

        /// Transact `edn` in session `c` (autocommit); returns the report.
        fn t(&self, c: &str, edn: &str) -> serde_json::Value {
            let r = get(
                c,
                &format!("t('{}', '{}')", self.store, edn.replace('\'', "''")),
            );
            serde_json::from_str(&r).unwrap_or_else(|e| panic!("{edn}: {r}: {e}"))
        }

        /// A: BEGIN; t(a) -- B: t(b) in the background -- wait for B to block
        /// or finish -- A: COMMIT. Returns (B waited on a lock, B's error or "").
        fn race(&self, a: &str, b: &str) -> (bool, String) {
            on("cw_a", "BEGIN");
            on(
                "cw_a",
                &format!("DO $d$ BEGIN PERFORM t('{}', '{a}'); END $d$", self.store),
            );
            let pid_b = get("cw_b", "pg_backend_pid()");
            Spi::run(&format!(
                "SELECT dblink_send_query('cw_b', $q$SELECT t('{}', '{b}')::text$q$)",
                self.store
            ))
            .expect("send");
            let mut waited = false;
            for _ in 0..100 {
                waited = text(&format!(
                    "SELECT wait_event_type FROM pg_stat_activity WHERE pid = {pid_b}"
                )) == "Lock";
                let busy: i32 = Spi::get_one("SELECT dblink_is_busy('cw_b')")
                    .unwrap()
                    .unwrap();
                if waited || busy == 0 {
                    break;
                }
                Spi::run("SELECT pg_sleep(0.05)").unwrap();
            }
            on("cw_a", "COMMIT");
            let err = text(
                "SELECT pg_temp.capture($q$SELECT * FROM dblink_get_result('cw_b') AS t(r text)$q$)",
            );
            // Drain the end-of-results marker so the connection can run more.
            let _ = text(
                "SELECT pg_temp.capture($q$SELECT * FROM dblink_get_result('cw_b') AS t(r text)$q$)",
            );
            (waited, err)
        }

        /// The live values of (e, a) in this store, sorted, as text.
        fn live(&self, table: &str, e: i64, attr: &str) -> Vec<String> {
            let s = get(
                "cw_a",
                &format!(
                    "SELECT coalesce(string_agg(v::text, ',' ORDER BY v::text), '') \
                     FROM mentat_{st}.{table} c JOIN mentat_{st}.idents i ON i.entid = c.a \
                     WHERE c.e = {e} AND i.ident = ':{attr}'",
                    st = self.store
                ),
            );
            s.split(',')
                .filter(|v| !v.is_empty())
                .map(String::from)
                .collect()
        }

        fn finish(self) {
            self.drop_store();
            Spi::run("SELECT dblink_disconnect('cw_a'), dblink_disconnect('cw_b')").expect("close");
        }
    }

    fn tempid(report: &serde_json::Value, name: &str) -> i64 {
        report["tempids"][name].as_i64().expect("tempid")
    }

    /// Two replacements of one cardinality-one value: B waits for A, then
    /// replaces A's value. One live value, B's.
    #[pg_test]
    fn concurrent_replacements_leave_one_value() {
        let Some(r) = Race::new("cw_replace") else {
            return;
        };
        r.t(
            "cw_a",
            "[{:db/ident :rc/n :db/valueType :db.type/long :db/cardinality :db.cardinality/one}]",
        );
        let e = tempid(&r.t("cw_a", "[{:db/id \"e\" :rc/n 1}]"), "e");
        let (waited, err) = r.race(
            &format!("[[:db/add {e} :rc/n 2]]"),
            &format!("[[:db/add {e} :rc/n 3]]"),
        );
        let live = r.live("current_long", e, "rc/n");
        r.finish();
        assert_eq!(err, "", "B commits");
        assert_eq!(live, vec!["3"], "one live value, the last writer's");
        assert!(waited, "B waited for A");
    }

    /// Two first assertions of one cardinality-one (e, a), which has no value
    /// to retract: B waits, then replaces A's.
    #[pg_test]
    fn concurrent_first_assertions_leave_one_value() {
        let Some(r) = Race::new("cw_empty") else {
            return;
        };
        r.t(
            "cw_a",
            "[{:db/ident :rc/n :db/valueType :db.type/long :db/cardinality :db.cardinality/one} \
              {:db/ident :rc/tag :db/valueType :db.type/string :db/cardinality :db.cardinality/one}]",
        );
        let e = tempid(&r.t("cw_a", "[{:db/id \"e\" :rc/tag \"t\"}]"), "e");
        let (waited, err) = r.race(
            &format!("[[:db/add {e} :rc/n 2]]"),
            &format!("[[:db/add {e} :rc/n 3]]"),
        );
        let live = r.live("current_long", e, "rc/n");
        r.finish();
        assert_eq!(err, "", "B commits");
        assert_eq!(live, vec!["3"]);
        assert!(waited, "B waited for A");
    }

    /// The #286 alteration many -> one against a concurrent second value: the
    /// writer waits for the alteration, then sees the attribute is
    /// cardinality one and replaces the value.
    #[pg_test]
    fn alteration_to_cardinality_one_waits_out_writers() {
        let Some(r) = Race::new("cw_alter") else {
            return;
        };
        r.t(
            "cw_a",
            "[{:db/ident :race/t :db/valueType :db.type/string :db/cardinality :db.cardinality/many}]",
        );
        let e = tempid(&r.t("cw_a", "[{:db/id \"e\" :race/t [\"x\"]}]"), "e");
        let (waited, err) = r.race(
            "[[:db/add :race/t :db/cardinality :db.cardinality/one]]",
            &format!("[[:db/add {e} :race/t \"y\"]]"),
        );
        let live = r.live("current_text", e, "race/t");
        let card = get(
            "cw_a",
            "SELECT cardinality FROM mentat_cw_alter.schema WHERE ident = ':race/t'",
        );
        r.finish();
        assert_eq!(err, "", "B commits");
        assert_eq!(card, "one");
        assert_eq!(live, vec!["y"], "B replaced x");
        assert!(waited, "B waited for the alteration");
    }

    /// Two cas calls from the same old value: B waits, then sees A's value and
    /// fails. One live value, A's.
    #[pg_test]
    fn concurrent_cas_from_one_old_value_fails_the_second() {
        let Some(r) = Race::new("cw_cas") else {
            return;
        };
        r.t(
            "cw_a",
            "[{:db/ident :rc/k :db/valueType :db.type/long :db/cardinality :db.cardinality/one}]",
        );
        let e = tempid(&r.t("cw_a", "[{:db/id \"e\" :rc/k 5}]"), "e");
        let (waited, err) = r.race(
            &format!("[[:db/cas {e} :rc/k 5 7]]"),
            &format!("[[:db/cas {e} :rc/k 5 6]]"),
        );
        let live = r.live("current_long", e, "rc/k");
        r.finish();
        assert!(err.contains("cas"), "B's cas fails: {err:?}");
        assert_eq!(live, vec!["7"]);
        assert!(waited, "B waited for A");
    }

    /// A no-op cas (5 -> 5) still guards: a concurrent cas 5 -> 6 waits for it,
    /// and then goes ahead (the value is still 5).
    #[pg_test]
    fn noop_cas_holds_its_guard() {
        let Some(r) = Race::new("cw_cas_noop") else {
            return;
        };
        r.t(
            "cw_a",
            "[{:db/ident :rc/k :db/valueType :db.type/long :db/cardinality :db.cardinality/one}]",
        );
        let e = tempid(&r.t("cw_a", "[{:db/id \"e\" :rc/k 5}]"), "e");
        let (waited, err) = r.race(
            &format!("[[:db/cas {e} :rc/k 5 5]]"),
            &format!("[[:db/cas {e} :rc/k 5 6]]"),
        );
        let live = r.live("current_long", e, "rc/k");
        r.finish();
        assert_eq!(err, "", "B commits");
        assert_eq!(live, vec!["6"]);
        assert!(waited, "B waited for A's guard");
    }

    /// Two new entities asserting one :db.unique/identity value: B waits and
    /// upserts into A's entity (the unique lock was already there; now it is
    /// taken before the upsert lookup, so B upserts instead of failing).
    #[pg_test]
    fn concurrent_identity_assertions_upsert() {
        let Some(r) = Race::new("cw_unique") else {
            return;
        };
        r.t(
            "cw_a",
            "[{:db/ident :u/email :db/valueType :db.type/string :db/cardinality :db.cardinality/one \
               :db/unique :db.unique/identity} \
              {:db/ident :u/n :db/valueType :db.type/long :db/cardinality :db.cardinality/one}]",
        );
        let (waited, err) = r.race(
            "[{:db/id \"a\" :u/email \"u\" :u/n 1}]",
            "[{:db/id \"b\" :u/email \"u\" :u/n 2}]",
        );
        let holders = get(
            "cw_a",
            "SELECT count(*) FROM mentat_cw_unique.current_text WHERE v = 'u'",
        );
        let n = get(
            "cw_a",
            "SELECT string_agg(n.v::text, ',') FROM mentat_cw_unique.current_text u \
             JOIN mentat_cw_unique.current_long n ON n.e = u.e WHERE u.v = 'u'",
        );
        r.finish();
        assert_eq!(err, "", "B commits");
        assert_eq!(holders, "1", "one entity holds the value");
        assert_eq!(n, "2", "B upserted into A's entity");
        assert!(waited, "B waited for A");
    }

    /// Two transactions that each install schema, then write (#413). A's first
    /// call takes the store's generation row before it installs anything; B's
    /// first schema call waits for it there, holding nothing A needs, so A's
    /// later install of B's ident and its data go through. Without that order
    /// B held its uncommitted ident and waited for the row at the end of its
    /// call, A waited for B's ident, and PostgreSQL aborted one (40P01).
    #[pg_test]
    fn schema_then_data_transactions_do_not_deadlock() {
        let Some(r) = Race::new("cw_schema") else {
            return;
        };
        let st = &r.store;
        let schema = |ident: &str| {
            format!(
                "[{{:db/ident :cs/{ident} :db/valueType :db.type/long \
                 :db/cardinality :db.cardinality/one}}]"
            )
        };
        // Session c's t(edn), with its error ("" if none).
        let try_t = |c: &str, edn: &str| {
            let sql = format!("DO $d$ BEGIN PERFORM t('{st}', '{edn}'); END $d$");
            text(&format!(
                "SELECT pg_temp.capture($q$SELECT dblink_exec('{c}', {}) $q$)",
                pgrx::spi::quote_literal(&sql)
            ))
        };
        on("cw_a", "BEGIN");
        on("cw_b", "BEGIN");
        let a1 = try_t("cw_a", &schema("x"));
        let pid_b = get("cw_b", "pg_backend_pid()");
        Spi::run(&format!(
            "SELECT dblink_send_query('cw_b', $q$SELECT t('{st}', '{}')::text$q$)",
            schema("y")
        ))
        .expect("send");
        let mut waited = false;
        for _ in 0..100 {
            waited = text(&format!(
                "SELECT wait_event_type FROM pg_stat_activity WHERE pid = {pid_b}"
            )) == "Lock";
            let busy: i32 = Spi::get_one("SELECT dblink_is_busy('cw_b')")
                .unwrap()
                .unwrap();
            if waited || busy == 0 {
                break;
            }
            Spi::run("SELECT pg_sleep(0.05)").unwrap();
        }
        let a2 = try_t("cw_a", &schema("y"));
        let a3 = try_t("cw_a", "[{:cs/x 1 :cs/y 2}]");
        on("cw_a", "ROLLBACK");
        let b1 = text(
            "SELECT pg_temp.capture($q$SELECT * FROM dblink_get_result('cw_b') AS t(r text)$q$)",
        );
        let _ = text(
            "SELECT pg_temp.capture($q$SELECT * FROM dblink_get_result('cw_b') AS t(r text)$q$)",
        );
        let b2 = try_t("cw_b", "[{:cs/y 3}]");
        on("cw_b", "ROLLBACK");
        r.finish();
        assert_eq!(
            [a1, a2, a3, b1, b2],
            ["", "", "", "", ""].map(String::from),
            "no call fails (A: schema x, schema y, data; B: schema y, data)"
        );
        assert!(waited, "B's schema call waited for A's");
    }

    /// Writers of different (e, a) don't wait for each other.
    #[pg_test]
    fn writers_of_different_values_do_not_wait() {
        let Some(r) = Race::new("cw_apart") else {
            return;
        };
        r.t(
            "cw_a",
            "[{:db/ident :rc/n :db/valueType :db.type/long :db/cardinality :db.cardinality/one}]",
        );
        let e = tempid(&r.t("cw_a", "[{:db/id \"e\" :rc/n 1}]"), "e");
        let f = tempid(&r.t("cw_a", "[{:db/id \"f\" :rc/n 1}]"), "f");
        let (waited, err) = r.race(
            &format!("[[:db/add {e} :rc/n 2]]"),
            &format!("[[:db/add {f} :rc/n 3]]"),
        );
        r.finish();
        assert_eq!(err, "", "B commits");
        assert!(!waited, "B did not wait");
    }

    /// The advisory locks a transaction holds, by namespace (classid) and mode.
    fn held_locks() -> Vec<(String, String, i64)> {
        Spi::connect(|c| {
            c.select(
                "SELECT to_hex(classid::int8), mode, count(*) FROM pg_locks \
                 WHERE pid = pg_backend_pid() AND locktype = 'advisory' AND objsubid = 2 \
                 GROUP BY 1, 2 ORDER BY 1, 2",
                None,
                &[],
            )
            .unwrap()
            .map(|r| {
                (
                    r.get::<String>(1).unwrap().unwrap(),
                    r.get::<String>(2).unwrap().unwrap(),
                    r.get::<i64>(3).unwrap().unwrap(),
                )
            })
            .collect()
        })
    }

    /// Per-namespace lock counts gained since `before`.
    fn gained(before: &[(String, String, i64)]) -> Vec<String> {
        held_locks()
            .into_iter()
            .filter_map(|(ns, mode, n)| {
                let was = before
                    .iter()
                    .find(|(b, m, _)| *b == ns && *m == mode)
                    .map_or(0, |b| b.2);
                (n > was).then(|| format!("{ns} {mode} +{}", n - was))
            })
            .collect()
    }

    /// One transaction's keys, in one backend (a pgrx test is one transaction,
    /// so counts are deltas): each written attribute shared, each cardinality-one
    /// (e, a) and unique value exclusive. Over the budget (max_locks_per_transaction
    /// per-value keys) the attributes are locked exclusive instead, and the
    /// transaction still commits.
    #[pg_test]
    fn transaction_takes_value_keys_or_whole_attribute() {
        crate::ensure_extension_loaded();
        Spi::run(
            "SELECT edn_t('[{:db/ident :bl/email :db/valueType :db.type/string \
               :db/cardinality :db.cardinality/one :db/unique :db.unique/identity} \
               {:db/ident :bl/n :db/valueType :db.type/long :db/cardinality :db.cardinality/one}]')",
        )
        .unwrap();
        // ATTR "Mnt1", UNIQUE "Mnt2", EA "Mnt3".
        let before = held_locks();
        Spi::run("SELECT edn_t('[{:db/id \"a\" :bl/email \"a@x\" :bl/n 1}]')").unwrap();
        assert_eq!(
            gained(&before),
            vec![
                "4d6e7431 ShareLock +2",
                "4d6e7432 ExclusiveLock +1",
                "4d6e7433 ExclusiveLock +2"
            ]
        );
        let budget: i64 = Spi::get_one("SELECT current_setting('max_locks_per_transaction')::int8")
            .unwrap()
            .unwrap();
        let tx: String = (0..budget)
            .map(|i| format!("{{:bl/email \"b{i}@x\" :bl/n {i}}}"))
            .collect::<Vec<_>>()
            .join(" ");
        let before = held_locks();
        let report = Spi::get_one::<String>(&format!("SELECT edn_t('[{tx}]')"))
            .unwrap()
            .unwrap();
        assert!(
            report.contains(&format!("\"datoms_asserted\":{}", 2 * budget)),
            "{report}"
        );
        // Both attributes whole, exclusive; no per-value keys.
        assert_eq!(gained(&before), vec!["4d6e7431 ExclusiveLock +2"]);
    }
}
