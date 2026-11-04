// The 1.18.0 -> 1.19.1 upgrade's upgrade-locks section (#428): it takes every
// lock the script needs before anything else, without waiting while it holds
// one, and gives up with a "busy" error if a reader or writer keeps a table.
// The 1.19.0 script took them one by one and deadlocked with a mentat.t() that
// started in between. test/upgrade_with_writers.sh runs the real ALTER
// EXTENSION UPDATE against a writer loop.
//
// Each case runs in a database of its own, through two dblink sessions: the
// other tests share the test database and lock its mentat tables. Skipped
// (passes) without dblink.

#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod tests {
    use pgrx::prelude::*;

    const SCRIPT: &str = include_str!("../sql/pg_mentat--1.18.0--1.19.1.sql");

    fn text(sql: &str) -> String {
        Spi::get_one::<String>(sql)
            .unwrap_or_else(|e| panic!("{sql}: {e}"))
            .unwrap_or_default()
    }

    /// The DO $locks$ block, giving up after `patience` seconds.
    fn lock_block(patience: u32) -> String {
        let start = SCRIPT.find("DO $locks$").expect("lock block");
        let end = SCRIPT[start..].find("$locks$;").unwrap() + start + "$locks$;".len();
        let block = &SCRIPT[start..end];
        assert!(block.contains("patience NUMERIC := 30;"));
        block.replace(
            "patience NUMERIC := 30;",
            &format!("patience NUMERIC := {patience};"),
        )
    }

    /// Run `sql` in session `c` (autocommit, or inside its open transaction).
    fn on(c: &str, sql: &str) {
        Spi::run_with_args("SELECT dblink_exec($1, $2)", &[c.into(), sql.into()])
            .unwrap_or_else(|e| panic!("{c}: {sql}: {e}"))
    }

    /// `SELECT <expr>` in session `c`, as text.
    fn get(c: &str, expr: &str) -> String {
        Spi::get_one_with_args::<String>(
            "SELECT r FROM dblink($1, $2) AS t(r TEXT)",
            &[c.into(), format!("SELECT ({expr})::TEXT").into()],
        )
        .unwrap_or_else(|e| panic!("{c}: {expr}: {e}"))
        .unwrap_or_default()
    }

    /// `sql` in session `c`: "" or its SQLSTATE and message.
    fn capture(c: &str, sql: &str) -> String {
        get(
            c,
            &format!("pg_temp.capture({})", pgrx::spi::quote_literal(sql)),
        )
    }

    /// The relation locks session `c` holds, "schema.rel Mode", sorted
    /// (read in `c`: its relations are in its database).
    fn held(c: &str) -> Vec<String> {
        let s = get(
            c,
            "SELECT coalesce(string_agg(r, ',' ORDER BY r), '') FROM ( \
               SELECT pg_catalog.format('%s.%s %s', n.nspname, c.relname, l.mode) AS r \
                 FROM pg_locks l JOIN pg_class c ON c.oid = l.relation \
                 JOIN pg_namespace n ON n.oid = c.relnamespace \
                WHERE l.pid = pg_backend_pid() AND l.locktype = 'relation' AND l.granted \
                  AND l.database = (SELECT oid FROM pg_database WHERE datname = current_database()) \
                  AND n.nspname LIKE 'mentat%') x",
        );
        s.split(',')
            .filter(|x| !x.is_empty())
            .map(String::from)
            .collect()
    }

    /// A fresh database `db` with pg_mentat and a named store lk_named, and
    /// sessions "up" (the upgrade) and "rd" (a client) on it. False without
    /// dblink. Each test has its own `db`: tests run in parallel.
    fn setup(db: &str) -> bool {
        crate::ensure_extension_loaded();
        let available: i64 =
            Spi::get_one("SELECT count(*) FROM pg_available_extensions WHERE name = 'dblink'")
                .unwrap()
                .unwrap();
        if available == 0 {
            return false;
        }
        Spi::run("CREATE EXTENSION IF NOT EXISTS dblink").expect("dblink");
        let conn = |db: &str| -> String {
            text(&format!(
                "SELECT pg_catalog.format('dbname=%s port=%s host=%s', '{db}', current_setting('port'), \
                   split_part(current_setting('unix_socket_directories'), ',', 1))"
            ))
        };
        Spi::run(&format!(
            "SELECT dblink_connect('lk_admin', '{}')",
            conn("postgres")
        ))
        .expect("connect");
        on("lk_admin", &format!("DROP DATABASE IF EXISTS {db}"));
        on("lk_admin", &format!("CREATE DATABASE {db}"));
        for c in ["up", "rd"] {
            Spi::run(&format!("SELECT dblink_connect('{c}', '{}')", conn(db))).expect("connect");
        }
        on("up", "CREATE EXTENSION pg_mentat");
        for c in ["up", "rd"] {
            on(c, "SET search_path TO mentat, public");
            on(
                c,
                "CREATE FUNCTION pg_temp.capture(stmt TEXT) RETURNS TEXT \
                 LANGUAGE plpgsql AS $$ BEGIN EXECUTE stmt; RETURN ''; \
                 EXCEPTION WHEN OTHERS THEN RETURN SQLSTATE || ' ' || SQLERRM; END $$",
            );
        }
        get("up", "bootstrap_schema()");
        get("up", "create_store('lk_named')");
        true
    }

    fn finish(db: &str) {
        Spi::run("SELECT dblink_disconnect('up'), dblink_disconnect('rd')").expect("close");
        on("lk_admin", &format!("DROP DATABASE {db}"));
        Spi::run("SELECT dblink_disconnect('lk_admin')").expect("close");
    }

    /// After the block, the transaction holds every lock a later section
    /// takes, for the default store and a named one, at least as strong; the
    /// settings it changes are back.
    #[pg_test]
    fn lock_block_takes_every_lock() {
        if !setup("mentat_ul_every") {
            return;
        }
        let path = get("up", "current_setting('search_path')");
        on("up", "BEGIN");
        on("up", &lock_block(30));
        let held = held("up");
        let mut want: Vec<String> = [
            "mentat.stores ShareLock",
            "mentat.cache_generation ExclusiveLock",
            "mentat.subscriptions ExclusiveLock",
            "mentat.materialized_views ExclusiveLock",
            "mentat.schema AccessExclusiveLock",
            "mentat.idents AccessExclusiveLock",
            "mentat.fulltext AccessExclusiveLock",
            "mentat.store_overview AccessExclusiveLock",
            "mentat.current_datoms AccessExclusiveLock",
            "mentat_lk_named.schema AccessExclusiveLock",
            "mentat_lk_named.idents AccessExclusiveLock",
            "mentat_lk_named.fulltext AccessExclusiveLock",
            "mentat_lk_named.facts AccessExclusiveLock",
            "mentat_lk_named.text_values AccessExclusiveLock",
            "mentat_lk_named.schema_summary AccessExclusiveLock",
        ]
        .map(String::from)
        .to_vec();
        for ty in [
            "ref", "boolean", "long", "double", "instant", "text", "keyword", "uuid", "bytes",
        ] {
            for s in ["mentat", "mentat_lk_named"] {
                want.push(format!("{s}.current_{ty} AccessExclusiveLock"));
                want.push(format!("{s}.datoms_{ty}_new AccessExclusiveLock"));
            }
        }
        for w in &want {
            assert!(held.contains(w), "{w} not held: {held:?}");
        }
        assert_eq!(get("up", "current_setting('lock_timeout')"), "0");
        assert_eq!(get("up", "current_setting('search_path')"), path);
        on("up", "ROLLBACK");
        finish("mentat_ul_every");
    }

    /// A reader keeps current_text for the whole patience: the block fails
    /// with the busy error (SQLSTATE 55P03), not a deadlock, and holds none of
    /// the locks it took on the way. The reader can then write: before #428
    /// that write deadlocked with the upgrade.
    #[pg_test]
    fn busy_store_fails_cleanly() {
        if !setup("mentat_ul_busy") {
            return;
        }
        // What the 1.19.0 report's writer held: a query's ACCESS SHARE.
        on("rd", "BEGIN");
        on(
            "rd",
            "DO $d$ BEGIN PERFORM count(*) FROM mentat.current_text; END $d$",
        );
        on("up", "BEGIN");
        let before = held("up");
        let t0 = std::time::Instant::now();
        let err = capture("up", &lock_block(1));
        let took = t0.elapsed();
        assert!(
            err.starts_with("55P03 pg_mentat upgrade: could not lock the stores' tables in 1 s"),
            "{err}"
        );
        assert!(err.contains("could not obtain lock on relation"), "{err}");
        assert!(took.as_secs_f64() >= 1.0, "gave up early: {took:?}");
        assert_eq!(held("up"), before, "a busy try kept a lock");
        assert_eq!(get("up", "current_setting('lock_timeout')"), "0");
        // The reader's own write goes through: nothing waits on it.
        on(
            "rd",
            "DO $d$ BEGIN PERFORM mentat.t('[{:db/doc \"upgrade-locks reader\"}]'); END $d$",
        );
        on("rd", "COMMIT");
        on("up", "ROLLBACK");
        finish("mentat_ul_busy");
    }

    /// A reader that ends while the block retries: the block then gets
    /// every lock.
    #[pg_test]
    fn lock_block_waits_out_a_short_reader() {
        if !setup("mentat_ul_short") {
            return;
        }
        on("rd", "BEGIN");
        on(
            "rd",
            "DO $d$ BEGIN PERFORM count(*) FROM mentat.current_ref; END $d$",
        );
        // The reader commits in the background, 0.4 s from now.
        Spi::run("SELECT dblink_send_query('rd', 'SELECT pg_sleep(0.4); COMMIT')").expect("send");
        on("up", "BEGIN");
        assert_eq!(capture("up", &lock_block(10)), "");
        assert!(
            held("up").contains(&"mentat.current_ref AccessExclusiveLock".to_string()),
            "{:?}",
            held("up")
        );
        on("up", "ROLLBACK");
        // One result per statement sent, then an empty one.
        for _ in 0..3 {
            Spi::run("SELECT count(*) FROM dblink_get_result('rd', false) AS t(r TEXT)")
                .expect("drain");
        }
        finish("mentat_ul_short");
    }
}
