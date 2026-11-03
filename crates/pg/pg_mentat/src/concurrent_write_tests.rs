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

    /// A store as 1.18.0 left it after re-asserting :color/red: the idents row
    /// points at a new entity N, the data at O. Returns (O, N).
    fn move_like_118(r: &Race, ident: &str) -> (i64, i64) {
        let sch = format!("mentat_{}", r.store);
        let o: i64 = get(
            "cw_a",
            &format!("SELECT entid FROM {sch}.idents WHERE ident = '{ident}'"),
        )
        .parse()
        .unwrap();
        let n: i64 = get("cw_a", &format!("nextval('{sch}.partition_user_seq')"))
            .parse()
            .unwrap();
        on(
            "cw_a",
            &format!(
                "UPDATE {sch}.idents SET entid = {n} WHERE ident = '{ident}'; \
                 UPDATE mentat.cache_generation SET gen = gen + 1 WHERE store_name = '{}'",
                r.store
            ),
        );
        (o, n)
    }

    /// A writer racing repair_moved_idents waits for it, then writes to the
    /// entity the repair pointed the ident back at, not to N: it resolves the
    /// ident only after the repair commits. Without the store key it didn't
    /// wait, wrote a ref to N with the idents it had cached, and once the
    /// repair committed moved_idents no longer listed the ident.
    #[pg_test]
    fn write_racing_repair_moved_idents_lands_on_the_owner() {
        let Some(r) = Race::new("cw_repair") else {
            return;
        };
        r.t(
            "cw_a",
            "[{:db/ident :c/color :db/valueType :db.type/ref :db/cardinality :db.cardinality/one} \
              {:db/ident :color/red}]",
        );
        let (o, n) = move_like_118(&r, ":color/red");
        // B's cache has :color/red -> N.
        let car = tempid(
            &r.t("cw_b", "[{:db/id \"car\" :c/color :color/red}]"),
            "car",
        );
        let sch = format!("mentat_{}", r.store);
        let color = |e: i64| {
            get(
                "cw_a",
                &format!("SELECT v FROM {sch}.current_ref WHERE e = {e}"),
            )
        };
        assert_eq!(color(car), n.to_string(), "the split, before the repair");
        on("cw_a", "BEGIN");
        let rep = get(
            "cw_a",
            &format!(
                "SELECT string_agg(action, ',') FROM mentat.repair_moved_idents('{}', false)",
                r.store
            ),
        );
        let pid_b = get("cw_b", "pg_backend_pid()");
        Spi::run(&format!(
            "SELECT dblink_send_query('cw_b', $q$SELECT (t('{}', '[{{:db/id \"x\" :c/color :color/red}}]')::jsonb -> 'tempids' ->> 'x')$q$)",
            r.store
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
        let x = text("SELECT * FROM dblink_get_result('cw_b') AS t(r text)");
        let _ = text(
            "SELECT pg_temp.capture($q$SELECT * FROM dblink_get_result('cw_b') AS t(r text)$q$)",
        );
        let x: i64 = x.parse().unwrap_or_else(|e| panic!("{x}: {e}"));
        let (car_color, x_color) = (color(car), color(x));
        let left = get(
            "cw_a",
            &format!("SELECT count(*) FROM mentat.moved_idents('{}')", r.store),
        );
        r.finish();
        assert_eq!(rep, "repaired");
        assert!(waited, "B waited for the repair");
        assert_eq!(
            car_color,
            o.to_string(),
            "the repair moved car's ref onto O"
        );
        assert_eq!(x_color, o.to_string(), "B's write landed on O, not N");
        assert_eq!(left, "0");
    }

    /// A real repair refuses REPEATABLE READ and SERIALIZABLE: its snapshot
    /// would predate the writers it waits for, so a ref one of them wrote to
    /// N would be left there with nothing listing it.
    #[pg_test]
    fn repair_moved_idents_refuses_a_snapshot_isolation_level() {
        let Some(r) = Race::new("cw_repair_iso") else {
            return;
        };
        r.t(
            "cw_a",
            "[{:db/ident :c/color :db/valueType :db.type/ref :db/cardinality :db.cardinality/one} \
              {:db/ident :color/red}]",
        );
        let _ = move_like_118(&r, ":color/red");
        on(
            "cw_a",
            "CREATE OR REPLACE FUNCTION pg_temp.capture(stmt TEXT) RETURNS TEXT \
             LANGUAGE plpgsql AS $$ BEGIN EXECUTE stmt; RETURN ''; \
             EXCEPTION WHEN OTHERS THEN RETURN SQLERRM; END $$",
        );
        let mut errs = Vec::new();
        for level in ["repeatable read", "serializable"] {
            on("cw_a", &format!("BEGIN ISOLATION LEVEL {level}"));
            errs.push(get(
                "cw_a",
                &format!(
                    "pg_temp.capture($q$SELECT * FROM mentat.repair_moved_idents('{}', false)$q$)",
                    r.store
                ),
            ));
            on("cw_a", "ROLLBACK");
        }
        // A dry run reads only, so it may run at any level.
        on("cw_a", "BEGIN ISOLATION LEVEL REPEATABLE READ");
        let dry = get(
            "cw_a",
            &format!(
                "SELECT string_agg(action, ',') FROM mentat.repair_moved_idents('{}', true)",
                r.store
            ),
        );
        on("cw_a", "ROLLBACK");
        let left = get(
            "cw_a",
            &format!("SELECT count(*) FROM mentat.moved_idents('{}')", r.store),
        );
        r.finish();
        for e in &errs {
            assert!(e.contains("must run under READ COMMITTED"), "{e}");
        }
        assert_eq!(dry, "would repair");
        assert_eq!(left, "1", "nothing repaired");
    }

    /// A schema-changing writer that holds the store key (it started first)
    /// and a repair that waits for it can't deadlock: the repair takes the key
    /// before the generation row, which the writer locks after the key.
    #[pg_test]
    fn schema_write_racing_repair_moved_idents_does_not_deadlock() {
        let Some(r) = Race::new("cw_repair2") else {
            return;
        };
        r.t(
            "cw_a",
            "[{:db/ident :c/color :db/valueType :db.type/ref :db/cardinality :db.cardinality/one} \
              {:db/ident :color/red}]",
        );
        move_like_118(&r, ":color/red");
        let st = r.store.clone();
        // B: a data call (store key shared), then the repair starts in A and
        // waits for B, then B changes schema (the generation row), commits.
        on("cw_b", "BEGIN");
        on(
            "cw_b",
            &format!("DO $d$ BEGIN PERFORM t('{st}', '[{{:db/id \"y\" :db/doc \"y\"}}]'); END $d$"),
        );
        let pid_a = get("cw_a", "pg_backend_pid()");
        Spi::run(&format!(
            "SELECT dblink_send_query('cw_a', $q$SELECT string_agg(action, ',') FROM mentat.repair_moved_idents('{st}', false)$q$)"
        ))
        .expect("send");
        let mut waited = false;
        for _ in 0..100 {
            waited = text(&format!(
                "SELECT wait_event_type FROM pg_stat_activity WHERE pid = {pid_a}"
            )) == "Lock";
            if waited {
                break;
            }
            Spi::run("SELECT pg_sleep(0.05)").unwrap();
        }
        let b = text(&format!(
            "SELECT pg_temp.capture($q$SELECT dblink_exec('cw_b', {}) $q$)",
            pgrx::spi::quote_literal(&format!(
                "DO $d$ BEGIN PERFORM t('{st}', '[{{:db/ident :c/size :db/valueType :db.type/long                  :db/cardinality :db.cardinality/one}}]'); END $d$"
            ))
        ));
        on("cw_b", "COMMIT");
        let rep = text(
            "SELECT pg_temp.capture($q$SELECT * FROM dblink_get_result('cw_a') AS t(r text)$q$)",
        );
        let _ = text(
            "SELECT pg_temp.capture($q$SELECT * FROM dblink_get_result('cw_a') AS t(r text)$q$)",
        );
        let left = get(
            "cw_a",
            &format!("SELECT count(*) FROM mentat.moved_idents('{st}')"),
        );
        let size = get(
            "cw_a",
            &format!("SELECT count(*) FROM mentat_{st}.schema WHERE ident = ':c/size'"),
        );
        r.finish();
        assert!(waited, "the repair waited for B's store key");
        assert_eq!(b, "", "B's schema call commits");
        assert_eq!(rep, "", "the repair finishes (no deadlock)");
        assert_eq!((left.as_str(), size.as_str()), ("0", "1"));
    }

    /// mentat_excise and rename_store lock the generation row too, so they take
    /// the store key first, as edn_t does. Before, `BEGIN; mentat_excise(..)`
    /// (generation row), then a repair queued for the key, then `t()` in the
    /// same transaction (wanting the key behind the repair) deadlocked.
    #[pg_test]
    fn excise_then_write_racing_repair_moved_idents_does_not_deadlock() {
        let Some(r) = Race::new("cw_repair3") else {
            return;
        };
        r.t(
            "cw_a",
            "[{:db/ident :c/color :db/valueType :db.type/ref :db/cardinality :db.cardinality/one} \
              {:db/ident :c/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one} \
              {:db/ident :color/red} {:db/ident :color/doomed}]",
        );
        move_like_118(&r, ":color/red");
        let st = r.store.clone();
        on(
            "cw_a",
            &format!(
                "UPDATE mentat_{st}.partitions SET allow_excision = true WHERE name = 'db.part/user'"
            ),
        );
        let doomed = get(
            "cw_a",
            &format!("SELECT entid FROM mentat_{st}.idents WHERE ident = ':color/doomed'"),
        );
        // B: excise an entity with an idents row (bumps the generation row).
        on("cw_b", "BEGIN");
        on(
            "cw_b",
            &format!(
                "DO $d$ BEGIN PERFORM mentat_excise(ARRAY[{doomed}]::bigint[], '{st}', 'x'); END $d$"
            ),
        );
        // A: the repair queues behind B.
        let pid_a = get("cw_a", "pg_backend_pid()");
        Spi::run(&format!(
            "SELECT dblink_send_query('cw_a', $q$SELECT string_agg(action, ',') FROM mentat.repair_moved_idents('{st}', false)$q$)"
        ))
        .expect("send");
        let mut waited = false;
        for _ in 0..100 {
            waited = text(&format!(
                "SELECT wait_event_type FROM pg_stat_activity WHERE pid = {pid_a}"
            )) == "Lock";
            if waited {
                break;
            }
            Spi::run("SELECT pg_sleep(0.05)").unwrap();
        }
        // B: a write in the same transaction, then commit.
        let b = text(&format!(
            "SELECT pg_temp.capture($q$SELECT dblink_exec('cw_b', {}) $q$)",
            pgrx::spi::quote_literal(&format!(
                "DO $d$ BEGIN PERFORM t('{st}', '[{{:c/name \"after\" :c/color :color/red}}]'); END $d$"
            ))
        ));
        let _ = text(&format!(
            "SELECT pg_temp.capture($q$SELECT dblink_exec('cw_b', {}) $q$)",
            pgrx::spi::quote_literal("COMMIT")
        ));
        let rep = text(
            "SELECT pg_temp.capture($q$SELECT * FROM dblink_get_result('cw_a') AS t(r text)$q$)",
        );
        let _ = text(
            "SELECT pg_temp.capture($q$SELECT * FROM dblink_get_result('cw_a') AS t(r text)$q$)",
        );
        let left = get(
            "cw_a",
            &format!("SELECT count(*) FROM mentat.moved_idents('{st}')"),
        );
        // The excise committed through dblink: drop its excision_log row, which
        // outlives the test's transaction (the 1.19.0 upgrade tests read that
        // table).
        on(
            "cw_a",
            &format!("DELETE FROM mentat.excision_log WHERE store_name = '{st}'"),
        );
        r.finish();
        assert!(waited, "the repair waited for B");
        assert_eq!(b, "", "B's write after the excise commits (no deadlock)");
        assert_eq!(rep, "", "the repair finishes (no deadlock)");
        assert_eq!(left, "0");
    }

    /// The store key the repair computes in plpgsql is the transactor's: an
    /// edn_t call holds it shared ("Mnt0" = 0x4d6e7430).
    #[pg_test]
    fn store_key_in_sql_matches_the_transactor() {
        crate::ensure_extension_loaded();
        Spi::run("SELECT edn_t('[{:db/id \"k\" :db/doc \"k\"}]')").unwrap();
        let held: String = Spi::get_one(
            "SELECT string_agg(to_hex(classid::int8) || ' ' || to_hex(objid::int8) || ' ' || mode, ',') \
               FROM pg_locks WHERE pid = pg_backend_pid() AND locktype = 'advisory' \
                AND objsubid = 2 AND classid = 1299084336",
        )
        .unwrap()
        .unwrap();
        let (ns, h) = crate::functions::write_locks::store_key(0);
        assert_eq!(held, format!("{ns:x} {h:x} ShareLock"));
        // The repair's computation, from the function's source.
        let src = include_str!("../sql/32_moved_idents.sql");
        let a = src.find("        h := 2166136261;").unwrap();
        let b = src[a..].find("        -- Then make").unwrap() + a;
        let block = format!(
            "DO $d$ DECLARE sid BIGINT := 0; h BIGINT; BEGIN {} END $d$",
            &src[a..b]
        );
        Spi::run(&block).unwrap();
        let mine: String = Spi::get_one(
            "SELECT string_agg(to_hex(classid::int8) || ' ' || to_hex(objid::int8), ',') \
               FROM pg_locks WHERE pid = pg_backend_pid() AND locktype = 'advisory' \
                AND objsubid = 2 AND classid = 1299084336 AND mode = 'ExclusiveLock'",
        )
        .unwrap()
        .unwrap();
        assert_eq!(mine, format!("{ns:x} {h:x}"), "the key edn_t took shared");
        for sid in [1_i64, 7, 1 << 40, -1] {
            let block = format!(
                "DO $d$ DECLARE sid BIGINT := {sid}; h BIGINT; BEGIN {} \
                 PERFORM set_config('pgm.h', h::text, true); END $d$",
                &src[a..src[a..]
                    .find("        PERFORM pg_advisory_xact_lock")
                    .unwrap()
                    + a]
            );
            Spi::run(&block).unwrap();
            let h: i64 = Spi::get_one("SELECT current_setting('pgm.h')::int8")
                .unwrap()
                .unwrap();
            assert_eq!(
                h,
                i64::from(crate::functions::write_locks::store_key(sid).1),
                "store_id {sid}"
            );
        }
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
