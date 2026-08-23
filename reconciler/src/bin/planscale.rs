//! How the control loop scales with nodes and apps.
//!
//! `plan()` is a pure function that runs on every reconciler pass over the WHOLE
//! world — every manifest, every node's inventory. That makes it the one part of
//! the platform whose scaling can be measured exactly, with no fleet, no NATS and
//! no guessing, so it is measured rather than reasoned about.
//!
//! Also reports the inventory payload per node, because the snapshot-not-delta
//! decision in `plan.rs` names a ceiling (the NATS max payload) and nobody had
//! put a number next to it.
//!
//! `comp-planscale [--nodes 10,100,1000] [--apps 100,1000,10000]`

use std::collections::BTreeMap;
use std::time::Instant;

use comp_reconciler::plan::{
    plan, Capacity, Cfg, Component, Hysteresis, Ingress, Manifest, NodeInventory, RunningInstance,
    Strategy,
};

fn list(args: &[String], flag: &str, default: &[usize]) -> Vec<usize> {
    match args.iter().position(|a| a == flag) {
        Some(i) => args[i + 1].split(',').map(|s| s.trim().parse().unwrap()).collect(),
        None => default.to_vec(),
    }
}

/// `apps` apps spread over the fleet, each two replicas of one component.
///
/// Tenants are `apps / 10`, so the org count grows with the app count rather
/// than staying at one — a single tenant would hide any per-tenant cost.
fn world(nodes: usize, apps: usize) -> (Vec<Manifest>, Vec<NodeInventory>) {
    let digest = |a: usize| format!("sha256:{:064x}", a % 50); // 50 distinct artifacts
    let desired: Vec<Manifest> = (0..apps)
        .map(|a| Manifest {
            app: format!("app{a}"),
            tenant: format!("org{}", a % (apps / 10).max(1)),
            strategy: Strategy::Fused,
            components: vec![Component {
                id: "gate".into(),
                digest: digest(a),
                replicas: 2,
                scale: None,
                placement: Default::default(),
                host_needs: vec!["wasi:keyvalue/store@0.2.0-draft".into()],
                config: BTreeMap::new(),
                secrets: Vec::new(),
                egress: Vec::new(),
            }],
            links: Vec::new(),
            ingress: Some(Ingress {
                host: format!("app{a}.example.com"),
                component: "gate".into(),
            }),
        })
        .collect();

    // Converged: each app's two replicas on two adjacent nodes, round-robin. This
    // is the steady state the loop actually spends its life in — the pass that
    // changes nothing still costs a full diff.
    let mut per_node: Vec<Vec<RunningInstance>> = vec![Vec::new(); nodes];
    for (a, m) in desired.iter().enumerate() {
        for r in 0..2 {
            per_node[(a * 2 + r) % nodes].push(RunningInstance {
                tenant: m.tenant.clone(),
                app: m.app.clone(),
                component: "gate".into(),
                digest: digest(a),
                count: 1,
                ingress_host: Some(format!("app{a}.example.com")),
            });
        }
    }
    let observed: Vec<NodeInventory> = (0..nodes)
        .map(|n| NodeInventory {
            node: format!("n{n}"),
            labels: BTreeMap::new(),
            host_ifaces: vec!["wasi:keyvalue/store@0.2.0-draft".into()],
            kv_shared: true,
            address: format!("10.0.0.{}:8080", n % 250),
            capacity: Capacity { cpus: 8 },
            instances: std::mem::take(&mut per_node[n]),
        })
        .collect();
    (desired, observed)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let node_counts = list(&args, "--nodes", &[10, 100, 1000]);
    let app_counts = list(&args, "--apps", &[100, 1000, 10_000]);
    let cfg = Cfg::default();

    println!("\n=== plan() over the whole world, converged ===\n");
    println!("  nodes   apps  insts │  cold ms  steady ms  parse ms recheck │ inv KiB/node  read MiB/pass │ cmds");
    for &nodes in &node_counts {
        for &apps in &app_counts {
            let (desired, observed) = world(nodes, apps);

            // ONE hysteresis across every run, because that is what the loop has:
            // it is created at startup and lives forever. A fresh one per run
            // measures only the cold pass — the pass after a reconciler restart or
            // a node joining — and never the steady state the loop actually spends
            // its life in. Both are reported, because they are different questions.
            let mut hyst = Hysteresis::default();
            let t = Instant::now();
            let out = plan(&desired, &observed, None, &mut hyst, &cfg);
            let cold = t.elapsed().as_secs_f64() * 1000.0;

            let mut runs: Vec<f64> = (0..5)
                .map(|_| {
                    let t = Instant::now();
                    let out = plan(&desired, &observed, None, &mut hyst, &cfg);
                    std::hint::black_box(&out);
                    t.elapsed().as_secs_f64() * 1000.0
                })
                .collect();
            runs.sort_by(|a, b| a.partial_cmp(b).unwrap());
            // What the loop pays BEFORE plan() even runs: pull every snapshot and
            // parse it. Polling re-does this every pass; a watched mirror would do
            // it once per change. Measured, because "read the whole world each
            // pass" only matters if reading is expensive next to planning.
            let wire: Vec<Vec<u8>> =
                observed.iter().map(|n| serde_json::to_vec(n).unwrap()).collect();
            let t = Instant::now();
            for raw in &wire {
                std::hint::black_box(serde_json::from_slice::<NodeInventory>(raw).unwrap());
            }
            let parse = t.elapsed().as_secs_f64() * 1000.0;
            // And what it costs to notice nothing changed. A snapshot is
            // identical between passes unless the node did something, so hashing
            // the bytes and reusing the previous parse is the same answer for a
            // fraction of the work — without a watch protocol, and without giving
            // up the TTL expiry that read_all's absence-is-death depends on.
            let t = Instant::now();
            for raw in &wire {
                use std::hash::{Hash, Hasher};
                let mut h = std::collections::hash_map::DefaultHasher::new();
                raw.hash(&mut h);
                std::hint::black_box(h.finish());
            }
            let recheck = t.elapsed().as_secs_f64() * 1000.0;

            let insts: usize = observed.iter().map(|n| n.instances.len()).sum();
            let biggest =
                observed.iter().map(|n| serde_json::to_vec(n).unwrap().len()).max().unwrap_or(0);
            let total: usize = observed.iter().map(|n| serde_json::to_vec(n).unwrap().len()).sum();

            println!(
                "  {nodes:5}  {apps:5}  {insts:5} │ {cold:8.2} {:9.2} {parse:9.2} {recheck:7.2} │ {:12.1}  {:13.2} │ {:4}",
                runs[2],
                biggest as f64 / 1024.0,
                total as f64 / (1024.0 * 1024.0),
                out.commands.len() + out.deferred,
            );
        }
    }
    println!(
        "\n  inv KiB/node is the JSON one host publishes every heartbeat; NATS refuses\n  \
         a message over 1 MiB by default, so that column is the snapshot ceiling.\n  \
         read MiB/pass is what the reconciler pulls from KV on EVERY pass.\n"
    );
}
