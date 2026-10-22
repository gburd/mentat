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
    for from in edges.keys().cloned().chain([default.to_string()]) {
        let (mut seen, mut todo) = (HashSet::new(), vec![from.clone()]);
        while let Some(v) = todo.pop() {
            if seen.insert(v.clone()) {
                todo.extend(edges.get(&v).into_iter().flatten().cloned());
            }
        }
        assert!(seen.contains(&newest), "no upgrade path {from} -> {newest}");
    }
}
