//! What a start costs, and that a corrupt cache cannot brick a node.
//!
//! This was `bench/coldstart/` — a bash script plus three Python files — and there
//! was never a reason for it to be shell: it drives the command bus, which is a Rust
//! trait, and reads timings the host prints itself. Shell is for orchestrating
//! processes across machines; nothing here leaves this one.
//!
//! It measures AND asserts. A benchmark nobody runs rots, and an assertion with no
//! number tells you it passed but not what it cost — so this prints ADR-0040's table
//! and fails if the cache stops paying for itself.

mod fleet;

use std::collections::BTreeMap;
use std::time::Duration;

use comp_lattice::nats::NatsLattice;
use comp_lattice::CommandBus;
use fleet::Fleet;
use serde_json::Value;

/// The start command the node last accepted, read from its own ledger — the same
/// bytes the reconciler sent, so replaying it is exactly a restart and not an
/// approximation of one.
fn ledger_entry(state_dir: &std::path::Path) -> (String, Value) {
    let path = state_dir.join("instances.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    let all: BTreeMap<String, Value> = serde_json::from_str(&text).unwrap();
    let (id, cmd) = all.into_iter().next().expect("the node is holding nothing");
    (id, cmd)
}

fn phases(log: &str) -> Vec<(u64, bool)> {
    log.lines()
        .filter_map(|l| {
            let rest = l.split(" in ").nth(1)?;
            let (total, tail) = rest.split_once(" us (")?;
            Some((total.trim().parse().ok()?, tail.contains("cache-load")))
        })
        .collect()
}

#[test]
fn a_cached_artifact_starts_far_faster_than_a_compile() {
    let fleet = Fleet::start("coldstart", &["fixtures/one-replica.yaml"], 1, None);
    assert!(fleet.serves("shop.eve.test", Duration::from_secs(90)), "never served");

    let node = fleet.state_dir().join("n1");
    let (_, start_cmd) = ledger_entry(&node);
    let stop_cmd = serde_json::json!({
        "tenant": start_cmd["tenant"], "app": start_cmd["app"], "component": start_cmd["component"]
    });

    let rt = tokio::runtime::Runtime::new().unwrap();
    let bus = rt
        .block_on(NatsLattice::connect(&fleet.nats_url, &fleet.lattice, Duration::from_secs(15)))
        .unwrap();

    // Stop and start by hand, with the reconciler's own commands. Every other
    // iteration also clears BOTH caches — the pulled .wasm and the compiled .cwasm —
    // so the run contains real cold starts rather than re-pulls that still hit the
    // compile cache.
    for i in 0..6 {
        rt.block_on(bus.send("n1", "stop", serde_json::to_vec(&stop_cmd).unwrap(), Duration::from_secs(30)))
            .expect("stop");
        if i % 2 == 1 {
            for d in ["artifacts", "cache"] {
                let dir = node.join(d);
                if dir.is_dir() {
                    for e in std::fs::read_dir(&dir).unwrap().filter_map(Result::ok) {
                        let _ = std::fs::remove_file(e.path());
                    }
                }
            }
        }
        rt.block_on(bus.send("n1", "start", serde_json::to_vec(&start_cmd).unwrap(), Duration::from_secs(60)))
            .expect("start");
    }

    let log = fleet.node_log("n1");
    let rows = phases(&log);
    let cold: Vec<u64> = rows.iter().filter(|(_, c)| !c).map(|(t, _)| *t).collect();
    let warm: Vec<u64> = rows.iter().filter(|(_, c)| *c).map(|(t, _)| *t).collect();
    assert!(!cold.is_empty() && !warm.is_empty(), "expected both kinds of start: {rows:?}");

    let med = |mut v: Vec<u64>| {
        v.sort_unstable();
        v[v.len() / 2] as f64 / 1000.0
    };
    let (c, w) = (med(cold.clone()), med(warm.clone()));
    println!("    cold (compiles): {c:.2} ms over {} start(s)", cold.len());
    println!("    warm (cached):   {w:.2} ms over {} start(s)", warm.len());
    println!("    {:.0}x", c / w.max(0.01));

    // Loose on purpose: the exact ratio is hardware, but "the cache is worth having"
    // is the property, and a regression that makes a warm start as slow as a compile
    // is what this must catch.
    assert!(w < c / 5.0, "a cached start ({w:.2} ms) should be far cheaper than a compile ({c:.2} ms)");
}

#[test]
fn a_corrupt_cache_falls_back_to_compiling() {
    // `deserialize_file` maps machine code straight in, so a cache written by another
    // wasmtime build — or a torn write — must be dropped rather than propagated. This
    // is the branch that could brick a node, which is worth more than testing the
    // happy path twice.
    let fleet = Fleet::start("corrupt", &["fixtures/one-replica.yaml"], 1, None);
    assert!(fleet.serves("shop.eve.test", Duration::from_secs(90)), "never served");

    let node = fleet.state_dir().join("n1");
    let (_, start_cmd) = ledger_entry(&node);
    let stop_cmd = serde_json::json!({
        "tenant": start_cmd["tenant"], "app": start_cmd["app"], "component": start_cmd["component"]
    });

    let rt = tokio::runtime::Runtime::new().unwrap();
    let bus = rt
        .block_on(NatsLattice::connect(&fleet.nats_url, &fleet.lattice, Duration::from_secs(15)))
        .unwrap();
    rt.block_on(bus.send("n1", "stop", serde_json::to_vec(&stop_cmd).unwrap(), Duration::from_secs(30)))
        .expect("stop");

    let cache = node.join("cache");
    let files: Vec<_> = std::fs::read_dir(&cache).unwrap().filter_map(Result::ok).collect();
    assert!(!files.is_empty(), "nothing was cached, so there is nothing to corrupt");
    for f in &files {
        std::fs::write(f.path(), b"this is not machine code, it is a sentence").unwrap();
    }

    rt.block_on(bus.send("n1", "start", serde_json::to_vec(&start_cmd).unwrap(), Duration::from_secs(60)))
        .expect("start after corruption");
    assert!(fleet.serves("shop.eve.test", Duration::from_secs(60)), "did not recover");

    let log = fleet.node_log("n1");
    assert!(log.contains("ignoring unusable"), "it should say it dropped the bad cache");
    let rewritten = std::fs::read_dir(&cache)
        .unwrap()
        .filter_map(Result::ok)
        .any(|e| e.metadata().map(|m| m.len() > 1000).unwrap_or(false));
    assert!(rewritten, "a good artifact should have replaced the corrupt one");
    println!("    recovered, logged the drop, and rewrote the cache");
}
