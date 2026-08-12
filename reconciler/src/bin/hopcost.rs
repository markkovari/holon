//! What one cross-node call costs, measured rather than remembered.
//!
//! ADR-0032 put the hop at ~4% and nothing has re-measured it since. ADR-0074
//! restored a test that the split graph still WORKS; this is the other half —
//! the price.
//!
//! ## Why this is a benchmark and not a test
//!
//! The number is machine-dependent and noisy. An assertion on it either fails on
//! a busy laptop or is so loose it asserts nothing, and the repo has enough
//! tests that pass for the wrong reason. So it prints, and a human decides
//! whether the number is still true.
//!
//! ## What makes the comparison fair
//!
//! Two arms, identical in every respect except placement:
//!
//! * the same three components, the same links, the same request;
//! * two host processes in BOTH arms — the baseline pins all three components to
//!   the web node and leaves the data node idle, rather than running one host,
//!   so the difference is not "one process versus two";
//! * `--kv memory`, because ADR-0057 measured JetStream round trips dominating
//!   the request path. A hop hidden under two of those is a hop nobody can see;
//! * `/api/ratelimit` with a DISTINCT KEY per request. Since ADR-0070 that is
//!   exactly one call into `shaper` plus local storage — but only when the key is
//!   uncontended. The rate limit is a compare-and-set retry loop and every retry
//!   calls `shaper` again, so hammering one key would price contention
//!   amplification instead of one hop.
//!
//! ```
//! cargo run --release --bin comp-hopcost -- --secs 10 --threads 20
//! ```

use std::time::Duration;

use clap::Parser;
use comp_reconciler::fleet::{repo_root, Fleet};

#[derive(Parser)]
#[command(name = "comp-hopcost", about = "What one cross-node call costs")]
struct Args {
    /// Seconds of load per arm.
    #[arg(long, default_value = "10")]
    secs: u64,
    /// Concurrent connections. Closed-loop: at a fixed concurrency, throughput is
    /// the honest comparison and mean latency is Little's law restating it.
    #[arg(long, default_value = "20")]
    threads: usize,
    /// Run each arm this many times and take the best, since a laptop's noise is
    /// one-sided — something else stealing the CPU can only make a run slower.
    #[arg(long, default_value = "3")]
    rounds: usize,
}

fn artifacts() -> Vec<String> {
    let raw = repo_root().join("components/target/wasm32-wasip2/release");
    [
        ("gate", raw.join("gate_domain.wasm")),
        ("record-store", raw.join("record_store.wasm")),
        ("shaper", raw.join("shaper.wasm")),
    ]
    .iter()
    .map(|(id, p)| {
        assert!(p.exists(), "missing {} — run `just build`", p.display());
        format!("{id}={}", p.display())
    })
    .collect()
}

/// One arm: bring a fleet up on `fixture`, load it, and return requests/second.
fn measure(name: &str, lattice: &str, fixture: &str, args: &Args) -> f64 {
    let fleet = Fleet::start_labelled_kv(
        lattice,
        &[fixture],
        &artifacts(),
        &["role=web", "role=data"],
        Some("memory"),
    );
    assert!(
        fleet.serves("split.alice.test", Duration::from_secs(120)),
        "{name}: never served"
    );
    // Load with a DISTINCT key per request, which the shared `Fleet::load` helper
    // cannot do — it drives one key, and one key is the wrong instrument here.
    //
    // `gate`'s rate limit is a compare-and-set retry loop, and every retry calls
    // `shaper` again. On a contended key the split arm therefore pays SEVERAL
    // hops per request while the baseline pays several local calls, and the ratio
    // measures contention amplification rather than one hop. Distinct keys make
    // it exactly one call each, which is the thing being priced.
    // Verify the arm is the arm it claims to be, BEFORE trusting a number from it.
    //
    // `shaper` used to be unconstrained in split-graph.yaml, so the planner could
    // put it beside `gate` — and since ADR-0070 `/api/ratelimit` calls only
    // `shaper`. A "split" arm with a local shaper has no hop in it, and this
    // printed -0.1% before anyone noticed.
    let (n1, n2) = (fleet.node_log("n1"), fleet.node_log("n2"));
    let shaper_remote = n2.contains("started alice/split/shaper");
    let wants_split = fixture.contains("split-graph");
    assert_eq!(
        shaper_remote, wants_split,
        "{name}: shaper is on the {} node, which is not what this arm measures.\n\
         A co-located arm needs it beside gate; a split arm needs it across the bus.",
        if shaper_remote { "data" } else { "web" }
    );
    assert!(n1.contains("started alice/split/gate"), "{name}: gate is not on the web node");
    println!(
        "    {name:<10} gate on n1, shaper on {}",
        if shaper_remote { "n2 (over wrpc)" } else { "n1 (in process)" }
    );

    let port = fleet.ingress_port;

    let run = |secs: u64| -> u64 {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let ok = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
        let mut hs = Vec::new();
        for w in 0..args.threads {
            let (stop, ok) = (stop.clone(), ok.clone());
            hs.push(std::thread::spawn(move || {
                // One client per worker, reused. A client per REQUEST opens a new
                // TCP connection every time, which costs both arms the same and
                // buries the thing being measured underneath it.
                let c = reqwest::blocking::Client::builder()
                    .timeout(Duration::from_secs(10))
                    .build()
                    .unwrap();
                let url = format!("http://127.0.0.1:{port}/api/ratelimit");
                let mut i = 0u64;
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    i += 1;
                    let sent = c
                        .post(&url)
                        .header("host", "split.alice.test")
                        .json(&serde_json::json!({
                            "key": format!("k-{w}-{i}"),
                            "capacity": 100_000_000u64,
                            "refill": 100_000_000u64
                        }))
                        .send()
                        .map(|r| r.status().is_success())
                        .unwrap_or(false);
                    if sent {
                        ok.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }));
        }
        std::thread::sleep(Duration::from_secs(secs));
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        for h in hs {
            let _ = h.join();
        }
        ok.load(std::sync::atomic::Ordering::Relaxed)
    };

    // A warm pass first: the first request through an instance pays its cold
    // start, and ADR-0040 measured that at 35 ms — enough to swamp the window.
    let _ = run(2);
    let ok = run(args.secs);
    let rps = ok as f64 / args.secs as f64;
    println!("    {name:<10} {rps:>9.0} rps   ({ok} answered)");
    rps
}

fn main() {
    let args = Args::parse();
    println!(
        "\ncomp-hopcost: one wRPC hop, {} threads, {}s per arm, best of {}\n",
        args.threads, args.secs, args.rounds
    );

    let (mut best_local, mut best_split) = (0.0f64, 0.0f64);
    for round in 1..=args.rounds {
        println!("  round {round}");
        best_local = best_local.max(measure(
            "co-located",
            &format!("hopl{round}"),
            "fixtures/colocated-graph.yaml",
            &args,
        ));
        best_split = best_split.max(measure(
            "split",
            &format!("hops{round}"),
            "fixtures/split-graph.yaml",
            &args,
        ));
    }

    let cost = 100.0 * (best_local - best_split) / best_local;
    // Closed loop at fixed concurrency, so mean service time is concurrency/rps.
    // The DIFFERENCE between the two is what one hop adds to a request.
    let t = |rps: f64| args.threads as f64 / rps * 1e6;
    let micros = t(best_split) - t(best_local);
    println!("\n  co-located {best_local:>7.0} rps   {:>7.0} us per request", t(best_local));
    println!("  split      {best_split:>7.0} rps   {:>7.0} us per request", t(best_split));
    println!("\n  one cross-node call adds {micros:.0} us, which is {cost:.1}% of THIS request");
    println!(
        "\n  The microseconds are the portable number; the percentage is not. It is a\n  \
         share of whatever else the request does, and this one deliberately does as\n  \
         little as possible — an in-memory store, one call, no work. Put a JetStream\n  \
         round trip in the path and the same hop is a far smaller fraction, which is\n  \
         how ADR-0032 and this can both be right (ADR-0057 made the same point about\n  \
         a latency column that was arithmetic).\n"
    );
}
