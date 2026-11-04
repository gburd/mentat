//! ALTER EXTENSION pg_mentat UPDATE needs a chain of sql/pg_mentat--A--B.sql
//! scripts from every released version to the newest one. A next-release
//! script that starts from an older patch release (1.17.1 -> 1.18.0 after
//! 1.17.2 shipped) leaves the latest release stranded.

#[test]
fn every_version_reaches_the_newest_by_upgrade_scripts() {
    use std::collections::{HashMap, HashSet};
    let ver = |s: &str| -> Vec<u32> { s.split('.').map(|n| n.parse().unwrap()).collect() };
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/sql");
    let mut edges: HashMap<String, Vec<String>> = HashMap::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        let name = entry.unwrap().file_name().into_string().unwrap();
        // pg_mentat--A--B.sql (pg_mentat--1.0.0.sql is an install script).
        let edge = name
            .strip_prefix("pg_mentat--")
            .and_then(|r| r.strip_suffix(".sql"))
            .and_then(|r| r.split_once("--"));
        if let Some((from, to)) = edge {
            edges.entry(from.into()).or_default().push(to.into());
        }
    }
    let newest = edges
        .values()
        .flatten()
        .max_by_key(|v| ver(v))
        .unwrap()
        .clone();
    let control = include_str!("../pg_mentat.control");
    let default = control
        .lines()
        .find_map(|l| l.strip_prefix("default_version = '"))
        .and_then(|l| l.strip_suffix('\''))
        .unwrap();
    // Every version any script mentions, as a start or a target, plus the
    // control file's default: a version that only appears as a target (the
    // latest release, before its next edge exists) must reach the newest too.
    let mut versions: HashSet<String> = edges.keys().cloned().collect();
    versions.extend(edges.values().flatten().cloned());
    versions.insert(default.to_string());
    for from in versions {
        let (mut seen, mut todo) = (HashSet::new(), vec![from.clone()]);
        while let Some(v) = todo.pop() {
            if seen.insert(v.clone()) {
                todo.extend(edges.get(&v).into_iter().flatten().cloned());
            }
        }
        assert!(seen.contains(&newest), "no upgrade path {from} -> {newest}");
    }
}

/// 1.18.0 -> 1.19.1 is the released 1.18.0 -> 1.19.0 script with the
/// upgrade-locks section in front (#428), and that script stays as released:
/// a 1.18.0 database updating to 1.19.0 or later must run the same SQL as
/// ones that already did, apart from the locking.
#[test]
fn the_1_19_1_edge_is_the_1_19_0_script() {
    let released = include_str!("../sql/pg_mentat--1.18.0--1.19.0.sql");
    let edge = include_str!("../sql/pg_mentat--1.18.0--1.19.1.sql");
    // FNV-1a of the file as tagged v1.19.0 (100677 bytes).
    let fnv = released.bytes().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
    });
    assert_eq!(
        (fnv, released.len()),
        (0x87843720521e9874, 100677),
        "pg_mentat--1.18.0--1.19.0.sql changed since v1.19.0"
    );
    let tail = |s: &'static str| &s[s.find("-- === pg-subs ===").unwrap()..];
    assert_eq!(tail(edge), tail(released));
    let head = &edge[..edge.find("-- === pg-subs ===").unwrap()];
    assert!(head.contains("-- === upgrade-locks (#428) ===\n"));
    // The lock section is the only thing ahead of the 1.19.0 script.
    assert_eq!(head.lines().filter(|l| l.starts_with("-- === ")).count(), 1);
}
