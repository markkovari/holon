//! One app's graph split over two machines, so every call between its components
//! crosses the bus.
//!
//! ADR-0032 measured this working and put the hop at ~4%, which is one of the
//! load-bearing claims in this repo: it is what makes "decompose an app into
//! components" affordable, and `CURRENT.md` leans on it. The script that proved
//! it — `bench/adversarial/split-graph.sh` — was deleted when other scenarios
//! became Rust tests, and nothing replaced it. `fixtures/split-graph.yaml` has
//! been sitting there since as the input to a test that did not exist.
//!
//! That is a claimed capability with no verification, which in this repo has a
//! track record: the secret reader was never linked (ADR-0061), `list-keys`
//! corrupted every key it returned (ADR-0068), and the conformance runner pointed
//! at a binary that had not existed for months. So: check it.
//!
//! ## What makes this a real cross-node test
//!
//! Placement has to be FORCED apart. `gate` is constrained to a node labelled
//! `role=web` and `record-store` to one labelled `role=data`; without labels the
//! planner is free to put all three on one node, and the test would pass while
//! proving nothing — every call would be an in-process call.
//!
//! Three things are asserted, and the first two are what make the third mean
//! something:
//!
//! 1. the components really are on different nodes (from inventory);
//! 2. `gate`'s host says it linked its imports **over wrpc**, so they resolved to
//!    remote instances rather than local ones;
//! 3. a real request through the ingress succeeds — which it cannot do unless the
//!    cross-node call works, since `gate` cannot rate-limit without `shaper` or
//!    persist without `record-store`.

use std::time::Duration;

use comp_reconciler::fleet::{repo_root, Fleet};

fn artifacts() -> Vec<String> {
    let raw = repo_root().join("components/target/wasm32-wasip2/release");
    // The RAW gate, not the composed one. `gate_domain.composed.wasm` already has
    // records and shaper inside it, so it has no imports left to satisfy — and a
    // "split graph" built from it would be one component in a trench coat.
    let parts = [
        ("gate", raw.join("gate_domain.wasm")),
        ("record-store", raw.join("record_store.wasm")),
        ("shaper", raw.join("shaper.wasm")),
    ];
    for (_, p) in &parts {
        assert!(p.exists(), "missing {} — run `just build`", p.display());
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

    // 1. Actually split. `role=web` is n1 and `role=data` is n2, so a placement
    //    that ignored constraints would put record-store on n1 and this passes
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

    // 2. gate's imports resolved to remote instances. Without this the test cannot
    //    tell a cross-node call from a co-located one.
    // BOTH of them, not just one. `gate` imports `records:store` and
    // `shaper:limit`; a run where only one resolved remotely would still contain
    // "over wrpc" and would still be half an in-process call.
    assert!(
        n1.contains("links 2 interface(s) over wrpc"),
        "gate did not link BOTH imports over wrpc — at least one was satisfied \
         locally, so that half never crossed a machine boundary:\n{n1}"
    );

    // 3. And it works: serving that request took a call to shaper and a call to
    //    record-store, both on the other node.
    // Printed so a reader can see WHAT matched, not just that something did.
    for line in n1.lines().chain(n2.lines()) {
        if line.contains("started alice/split") || line.contains("over wrpc") {
            println!("    {}", line.trim());
        }
    }
    println!("    the graph is split, the links are wrpc, and it serves");
}
