//! Moving a `:db/unique` value from one entity to another in one transaction
//! (reported 2026-11-18 against pg_mentat 1.20.0). An explicit entid never
//! upserts, as in Datomic: asserting a value another entity holds is a
//! `:db.error/unique-conflict`. Retracting it from the holder in the same
//! transaction frees it. pg_mentat's upsert_tests.rs
//! (test_upsert_move_unique_value_in_one_transaction) runs the same cases.

extern crate mentat;

use mentat::{Queryable, Store};

/// The entities holding `v` for attribute `a`, sorted.
fn holders(s: &Store, a: &str, v: &str) -> Vec<i64> {
    let q = format!(r#"[:find [?e ...] :where [?e {a} "{v}"]]"#);
    let mut es: Vec<i64> = s
        .q_once(&q, None)
        .unwrap_or_else(|e| panic!("{q}: {e:?}"))
        .results
        .into_coll()
        .expect("coll")
        .into_iter()
        .map(|b| b.into_entid().expect("entid"))
        .collect();
    es.sort();
    es
}

fn conflict(s: &mut Store, tx: &str) {
    let e = s.transact(tx).expect_err(tx).to_string();
    assert!(e.contains(":db.error/unique-conflict"), "{tx}: {e}");
}

#[test]
fn move_a_unique_value_in_one_transaction() {
    let mut s = Store::open("").expect("opened");
    s.transact(
        "[{:db/ident :mv/n :db/valueType :db.type/string :db/cardinality :db.cardinality/one}]",
    )
    .unwrap();
    for card in ["one", "many"] {
        for uniq in ["identity", "value"] {
            let a = format!(":mv/{card}-{uniq}");
            s.transact(&format!(
                "[{{:db/ident {a} :db/valueType :db.type/string \
                   :db/cardinality :db.cardinality/{card} :db/unique :db.unique/{uniq} \
                   :db/index true}}]"
            ))
            .unwrap();
            let moves = [
                r#"[[:db/retract S A "V"] [:db/add C A "V"]]"#,
                r#"[[:db/add C A "V"] [:db/retract S A "V"]]"#,
                r#"[[:db/retractEntity S] {:db/id C A "V"}]"#,
            ];
            for (i, mv) in moves.iter().enumerate() {
                let v = format!("v{i}");
                let r = s
                    .transact(&format!(
                        r#"[{{:db/id "c" :mv/n "C"}} {{:db/id "s" :mv/n "S" {a} "{v}"}}]"#
                    ))
                    .unwrap();
                let (c, sh) = (r.tempids["c"], r.tempids["s"]);
                // Alone, the assertion on C is a conflict.
                conflict(&mut s, &format!(r#"[[:db/add {c} {a} "{v}"]]"#));
                assert_eq!(holders(&s, &a, &v), vec![sh], "{a}");
                // With the retraction from S in the same transaction, it moves.
                let tx = mv
                    .replace('S', &sh.to_string())
                    .replace('C', &c.to_string())
                    .replace('A', &a)
                    .replace(r#""V""#, &format!(r#""{v}""#));
                s.transact(&tx).unwrap_or_else(|e| panic!("{tx}: {e}"));
                assert_eq!(holders(&s, &a, &v), vec![c], "{tx}");
            }
            // A tempid still upserts into the holder of an identity value.
            let h = s
                .transact(&format!(r#"[{{:db/id "h" :mv/n "H" {a} "w"}}]"#))
                .unwrap()
                .tempids["h"];
            if uniq == "identity" {
                let r = s
                    .transact(&format!(r#"[{{:db/id "x" {a} "w" :mv/n "X"}}]"#))
                    .unwrap();
                assert_eq!(r.tempids["x"], h, "{a}");
            } else {
                conflict(&mut s, &format!(r#"[{{:db/id "x" {a} "w"}}]"#));
            }
            // Two explicit entities can't both assert one new value.
            let r = s
                .transact(r#"[{:db/id "p" :mv/n "P"} {:db/id "q" :mv/n "Q"}]"#)
                .unwrap();
            let (p, q) = (r.tempids["p"], r.tempids["q"]);
            conflict(
                &mut s,
                &format!(r#"[[:db/add {p} {a} "z"] [:db/add {q} {a} "z"]]"#),
            );
            assert!(holders(&s, &a, "z").is_empty(), "{a}");
        }
    }
}
