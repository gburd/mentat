// Is sparsemap a better entid set than BTreeSet<i64> for mentat's hot paths?
// Shapes that mentat actually produces:
//  dense : entids allocated by one bulk tx  -> contiguous run in db.part/user
//  mixed : several txs interleaved over time -> long runs with gaps
//  sparse: entities touched by a selective query -> random across the partition
//  tiny  : the common pull/visited or cache v_es set -> a handful of ids
use sparsemap::SparseMap;
use std::collections::BTreeSet;
use std::hint::black_box;
use std::time::Instant;

const BASE: u64 = 1_000_001; // db.part/user start

fn lcg(s: &mut u64) -> u64 { *s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407); *s >> 11 }

fn shape(name: &str, n: usize) -> Vec<u64> {
    let mut s = 42u64;
    match name {
        "dense" => (0..n as u64).map(|i| BASE + i).collect(),
        "mixed" => { let mut v = Vec::with_capacity(n); let mut x = BASE; while v.len() < n { let run = 50 + lcg(&mut s) % 500; for _ in 0..run { if v.len() < n { v.push(x); x += 1; } } x += 1 + lcg(&mut s) % 5000; } v }
        "sparse" => { let mut v: Vec<u64> = (0..n).map(|_| BASE + lcg(&mut s) % 50_000_000).collect(); v.sort(); v.dedup(); v }
        "tiny" => (0..n as u64).map(|i| BASE + i * 37).collect(),
        _ => unreachable!(),
    }
}

fn time<F: FnMut()>(iters: u32, mut f: F) -> f64 { let t = Instant::now(); for _ in 0..iters { f(); } t.elapsed().as_secs_f64() * 1e9 / iters as f64 }

fn main() {
    println!("{:<7} {:>8} | {:>11} {:>11} | {:>9} {:>9} | {:>9} {:>9} | {:>9} {:>9}",
        "shape", "n", "bytes:bt", "bytes:sm", "build bt", "build sm", "probe bt", "probe sm", "and bt", "and sm");
    for (name, n) in [("tiny", 8usize), ("tiny", 64), ("dense", 100_000), ("dense", 1_000_000), ("mixed", 100_000), ("mixed", 1_000_000), ("sparse", 10_000), ("sparse", 100_000)] {
        let ids = shape(name, n);
        let other: Vec<u64> = ids.iter().step_by(3).copied().collect();
        let iters = if n <= 64 { 200_000 } else if n <= 100_000 { 20 } else { 3 };
        let build_bt = time(iters, || { let s: BTreeSet<u64> = ids.iter().copied().collect(); black_box(s); });
        let build_sm = time(iters, || { let s: SparseMap = ids.iter().copied().collect(); black_box(s); });
        let bt: BTreeSet<u64> = ids.iter().copied().collect();
        let sm: SparseMap = ids.iter().copied().collect();
        let bt2: BTreeSet<u64> = other.iter().copied().collect();
        let sm2: SparseMap = other.iter().copied().collect();
        // probe: 1000 lookups, half hits
        let mut s = 7u64; let probes: Vec<u64> = (0..1000).map(|i| if i % 2 == 0 { ids[(lcg(&mut s) as usize) % ids.len()] } else { BASE + lcg(&mut s) % 60_000_000 }).collect();
        let pi = if n <= 64 { 20_000 } else { 2_000 };
        let probe_bt = time(pi, || { let mut c = 0; for p in &probes { c += bt.contains(p) as u32; } black_box(c); }) / 1000.0;
        let probe_sm = time(pi, || { let mut c = 0; for p in &probes { c += sm.contains(*p) as u32; } black_box(c); }) / 1000.0;
        let and_bt = time(iters, || { let r: BTreeSet<u64> = bt.intersection(&bt2).copied().collect(); black_box(r); });
        let and_sm = time(iters, || { let r = &sm & &sm2; black_box(r); });
        // BTreeSet<u64> memory: ~ (B-tree node overhead) ~ 16-24 B/elt measured; use len*24 + node slack estimate via allocation-free proxy
        let bytes_bt = bt.len() * 24;
        let bytes_sm = sm.to_bytes().len();
        let f = |ns: f64| if ns >= 1e6 { format!("{:.2}ms", ns / 1e6) } else if ns >= 1e3 { format!("{:.1}us", ns / 1e3) } else { format!("{:.0}ns", ns) };
        println!("{:<7} {:>8} | {:>11} {:>11} | {:>9} {:>9} | {:>9} {:>9} | {:>9} {:>9}",
            name, ids.len(), bytes_bt, bytes_sm, f(build_bt), f(build_sm), f(probe_bt), f(probe_sm), f(and_bt), f(and_sm));
    }
}
