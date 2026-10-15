//! One app's graph split over two machines, so every call between its components crosses the bus.
//!
//! ADR-0032 measured the hop at ~4%, the load-bearing claim that makes "decompose an app into
//! components" affordable. The script that proved it (`bench/adversarial/split-graph.sh`) was deleted
//! and nothing replaced it; `fixtures/split-graph.yaml` has sat there as input to a test that did not
//! exist. Claimed capabilities here have a record of being unverified (ADR-0061, ADR-0068), so: check it.
//!
//! Placement has to be FORCED apart: `gate` is constrained to `role=web` and `record-store` to
//! `role=data`, or the planner may put all three on one node and every call would be in-process.
//!
//! 1. the components really are on different nodes (from inventory);
//! 2. `gate`'s host says it linked its imports **over wrpc**, resolving to remote instances;
//! 3. a real request through the ingress succeeds, which it cannot unless the cross-node call works.

use std::time::Duration;

use comp_reconciler::fleet::{repo_root, Fleet};

fn artifacts() -> Vec<String> {
    let raw = repo_root().join("components/target/wasm32-wasip2/release");
    // The RAW gate, not the composed one: `gate_domain.composed.wasm` has records and shaper inside, so
    // a "split graph" built from it would be one component in a trench coat.
    let parts = [
        ("gate", raw.join("gate_domain.wasm")),
        ("record-store", raw.join("record_store.wasm")),
        ("shaper", raw.join("shaper.wasm")),
    ];
    for (_, p) in &parts {
        assert!(p.exists(), "missing {} — run `cargo xtask build --force`", p.display());
    }
    parts.iter().map(|(id, p)| format!("{id}={}", p.display())).collect()
}

#[test]
fn one_apps_graph_spans_two_nodes_and_still_serves() {
    // Two nodes, one for each half of the graph.
    let fleet = Fleet::start_with_labels(
        "crossnode",
        &["fixtures/split-graph.yaml"],
        &artifacts(),
        &["role=web", "role=data"],
    );
    assert!(
        fleet.serves("split.alice.test", Duration::from_secs(120)),
        "the split app never served — which is either placement or the wrpc link, \
         and the logs below say which:\n--- n1 ---\n{}\n--- n2 ---\n{}\n--- reconciler ---\n{}",
        fleet.node_log("n1"),
        fleet.node_log("n2"),
        fleet.reconciler_log()
    );

    // 1. Actually split: a placement ignoring constraints would put record-store on n1 and pass
    //    for the wrong reason.
    let (n1, n2) = (fleet.node_log("n1"), fleet.node_log("n2"));
    assert!(n1.contains("started alice/split/gate"), "gate is not on the web node:\n{n1}");
    assert!(
        n2.contains("started alice/split/record-store"),
        "record-store is not on the data node:\n{n2}"
    );
    assert!(
        !n1.contains("started alice/split/record-store"),
        "record-store ALSO started on the web node, so gate could be calling a \
         local copy and nothing crosses the bus:\n{n1}"
    );

    // 2. gate's imports resolved to remote instances, which tells a cross-node call from a co-located
    // one. BOTH `records:store` and `shaper:limit`: one resolving remotely would still contain "over wrpc".
    assert!(
        n1.contains("links 2 interface(s) over wrpc"),
        "gate did not link BOTH imports over wrpc — at least one was satisfied \
         locally, so that half never crossed a machine boundary:\n{n1}"
    );

    // 3. And it works: serving took a call to shaper and to record-store, both on the other node.
    // Printed so a reader can see WHAT matched.
    for line in n1.lines().chain(n2.lines()) {
        if line.contains("started alice/split") || line.contains("over wrpc") {
            println!("    {}", line.trim());
        }
    }
    println!("    the graph is split, the links are wrpc, and it serves");
}
