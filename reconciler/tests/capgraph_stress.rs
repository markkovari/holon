//! What the merged graph costs, at sizes this repository has not reached yet (ADR-0091).
//!
//! The justifying query is a three-hop traversal (`app -> carries -> artifact -> imports -> interface`)
//! feeding a `CONTAINSANY` over the lesson pool; both halves grow. This reports a table of insert and
//! query times against node and edge counts, at the real size and three synthetic multiples.
//!
//! **Correctness at scale is asserted:** the join must return exactly the right lessons when the graph
//! is 20x and the pool 100x larger.
//! **Scaling SHAPE is asserted, loosely** (see `SUPERLINEAR_ALLOWANCE`): the thing to catch is an
//! accidental full scan per hop, an order of magnitude and not thirty percent.
//! **Absolute times are reported, never asserted:** they describe the machine (ADR-0019, ADR-0020).
//!
//! Run: `cargo test -p comp-reconciler --release --test capgraph_stress -- --ignored --nocapture`
//! Scales are tunable: `COMP_STRESS_SCALES=1,5,20,50 COMP_STRESS_LESSONS=50000 cargo test ...`
//!
//! `#[ignore]`d: it writes hundreds of thousands of rows and takes a minute or two.

use std::process::Command;
use std::time::Duration;

mod harness;
use harness::{Store, SURREAL_IMAGE};

/// How much bigger than the real catalogue each round is. `1` is the repository as it stands.
fn scales() -> Vec<usize> {
    std::env::var("COMP_STRESS_SCALES")
        .ok()
        .map(|v| v.split(',').filter_map(|s| s.trim().parse().ok()).collect::<Vec<_>>())
        .filter(|v: &Vec<usize>| !v.is_empty())
        .unwrap_or_else(|| vec![1, 5, 20])
}

/// How many lessons sit in the pool the join filters. The pool outgrows the graph.
fn lessons() -> usize {
    std::env::var("COMP_STRESS_LESSONS").ok().and_then(|v| v.parse().ok()).unwrap_or(20_000)
}

/// How much worse than linear the join may scale before this fails. Slack on purpose: a scan per hop
/// would land past 400x against 20x graph growth, an order of magnitude clear of this bound.
const SUPERLINEAR_ALLOWANCE: f64 = 8.0;

/// The interface the asserted lessons are tagged with. Real, so the assertion survives the synthetic
/// rows being removed.
const TAGGED: &str = "csv:codec/codec@0.1.0";
const APP: &str = "vet";

fn real_projection(generation: u64) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_comp-capgraph"))
        .args(["--format", "surql", "--gen", &generation.to_string()])
        .output()
        .expect("comp-capgraph did not run");
    assert!(out.status.success(), "comp-capgraph failed — is anything built?");
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// Synthetic rows in the same SHAPE as the projection, at `copies` times the size. Copies, not random
/// graphs, because traversal cost depends on fan-out and a copy of the real catalogue has the real
/// one. `copies - 1` extra copies are written; copy zero is the real one.
fn synthetic(copies: usize, generation: u64) -> String {
    let mut out = String::new();
    for c in 1..copies {
        // One app, its parts and their interfaces, per copy. Suffixed so the asserted join cannot
        // match a synthetic row.
        for a in 0..47 {
            out.push_str(&format!(
                "UPSERT app:⟨syn-{c}-{a}⟩ SET name = 'syn-{c}-{a}', root = 'syn-part-{c}-{a}-0', \
                 artifact = 'x.wasm', gen = {generation};\n"
            ));
            for p in 0..7 {
                let part = format!("syn-part-{c}-{a}-{p}");
                out.push_str(&format!(
                    "UPSERT artifact:⟨{part}⟩ SET name = '{part}', digest = 'sha256:syn', \
                     gen = {generation};\n"
                ));
                out.push_str(&format!(
                    "RELATE app:⟨syn-{c}-{a}⟩->carries:⟨{generation}|{c}|{a}|{p}⟩->\
                     artifact:⟨{part}⟩ SET gen = {generation};\n"
                ));
                for i in 0..3 {
                    // Interfaces are shared across the copy, like the real ones (`records:store/store`
                    // has 37 consumers); private interfaces would have no fan-in to traverse.
                    let iface = format!("syn:pkg-{c}-{}/thing@0.1.0", i + p % 5);
                    out.push_str(&format!(
                        "UPSERT interface:⟨{iface}⟩ SET name = '{iface}', exporter = '', \
                         consumers = 0, gen = {generation};\n"
                    ));
                    out.push_str(&format!(
                        "RELATE artifact:⟨{part}⟩->imports:⟨{generation}|{c}|{a}|{p}|{i}⟩->\
                         interface:⟨{iface}⟩ SET gen = {generation};\n"
                    ));
                }
            }
        }
    }
    out
}

/// The lesson pool. One in every `TAGGED_EVERY` is tagged with the real interface; the rest are noise.
const TAGGED_EVERY: usize = 500;

fn lesson_rows(n: usize) -> String {
    let mut out = String::new();
    for i in 0..n {
        let tag = if i % TAGGED_EVERY == 0 {
            TAGGED.to_string()
        } else {
            format!("noise:pkg-{i}/thing@0.1.0")
        };
        out.push_str(&format!(
            "UPSERT memory:⟨stress-{i}⟩ SET ns = 'errors', text = 'lesson {i}', \
             tags = ['{tag}'], uses = 0;\n"
        ));
    }
    out
}

/// Statements are posted in chunks: one huge body would measure HTTP upload, not the write path.
const CHUNK: usize = 2_000;

fn post_in_chunks(db: &Store, sql: &str) -> Duration {
    let lines: Vec<&str> = sql.lines().filter(|l| !l.trim_start().starts_with("--")).collect();
    let mut total = Duration::ZERO;
    for chunk in lines.chunks(CHUNK) {
        total += db.timed(&chunk.join("\n")).0;
    }
    total
}

/// The query under test, verbatim from the old `just lessons-for` recipe.
fn join_query(app: &str) -> String {
    format!(
        "LET $ifaces = (SELECT VALUE array::distinct(array::flatten(\
           ->carries->artifact->imports->interface.name)) FROM ONLY app:⟨{app}⟩);\n\
         SELECT text FROM memory WHERE tags CONTAINSANY $ifaces;"
    )
}

/// Median of five, so one scheduler hiccup does not become the reported number.
fn median_query(db: &Store, app: &str) -> (Duration, usize) {
    let mut times = Vec::new();
    let mut hits = 0;
    for _ in 0..5 {
        let (took, out) = db.timed(&join_query(app));
        hits = out.as_array().map(|a| a.len()).unwrap_or(0);
        times.push(took);
    }
    times.sort();
    (times[times.len() / 2], hits)
}

#[test]
#[ignore = "writes hundreds of thousands of rows; run with --ignored --nocapture"]
fn the_join_under_load() {
    let Some(db) = Store::start() else {
        eprintln!(
            "SKIPPED: could not start {SURREAL_IMAGE} — nothing about the cost of this \
             graph was measured by this run."
        );
        return;
    };

    // The pool first, and once: every round measures a graph growing against an already large pool,
    // the real shape.
    let n = lessons();
    let pool_write = post_in_chunks(&db, &lesson_rows(n));
    let expected_hits = n.div_ceil(TAGGED_EVERY);
    assert_eq!(
        db.count("memory") as usize,
        n,
        "the pool did not land — every timing below would be measuring an empty table"
    );
    println!(
        "\n  pool: {n} lessons written in {:?} ({:.0} rows/s), {expected_hits} tagged {TAGGED}",
        pool_write,
        n as f64 / pool_write.as_secs_f64().max(0.001)
    );

    println!(
        "\n  {:>6}  {:>9}  {:>9}  {:>12}  {:>12}  {:>7}",
        "scale", "nodes", "edges", "insert", "join (med)", "hits"
    );
    println!("  {}", "-".repeat(66));

    let mut first: Option<(f64, f64)> = None;
    let mut last_line = String::new();

    for (round, &scale) in scales().iter().enumerate() {
        let generation = (round + 1) as u64;

        let mut sql = real_projection(generation);
        sql.push_str(&synthetic(scale, generation));
        let insert = post_in_chunks(&db, &sql);

        let nodes = db.count("interface") + db.count("artifact") + db.count("app");
        let edges = db.count("imports") + db.count("exports") + db.count("carries");
        let (join, hits) = median_query(&db, APP);

        // CORRECTNESS, every round: the real app's answer must not change because the database grew.
        assert_eq!(
            hits, expected_hits,
            "at scale {scale} the join returned {hits} lessons, expected {expected_hits} — \
             the answer changed with the size of the database"
        );

        last_line = format!(
            "  {scale:>5}x  {nodes:>9}  {edges:>9}  {:>12}  {:>12}  {hits:>7}",
            format!("{:.2}s", insert.as_secs_f64()),
            format!("{:.1}ms", join.as_secs_f64() * 1000.0),
        );
        println!("{last_line}");

        if first.is_none() {
            first = Some(((nodes + edges) as f64, join.as_secs_f64()));
        }
    }

    // SCALING SHAPE, compared against the first round so one slow middle round cannot hide.
    if let Some((first_size, first_time)) = first {
        let size_now = (db.count("interface") + db.count("artifact") + db.count("app")) as f64
            + (db.count("imports") + db.count("exports") + db.count("carries")) as f64;
        let (time_now, _) = median_query(&db, APP);
        let grew = size_now / first_size.max(1.0);
        let slowed = time_now.as_secs_f64() / first_time.max(0.0001);

        println!(
            "\n  graph grew {grew:.1}x, the join slowed {slowed:.1}x \
             (allowance: {SUPERLINEAR_ALLOWANCE}x linear)"
        );
        assert!(
            slowed <= grew * SUPERLINEAR_ALLOWANCE,
            "the join slowed {slowed:.1}x while the graph grew {grew:.1}x — that is worse \
             than linear by more than {SUPERLINEAR_ALLOWANCE}x, which is what a full scan \
             per hop looks like"
        );
    }

    println!("\n  last round: {}\n", last_line.trim());
    println!(
        "  Times are this machine's, and are reported rather than asserted. What is \
         asserted is that the answer never changed and the shape stayed near-linear."
    );
}

/// What the lesson half of the join costs, and what a graph edge would cost instead.
///
/// The graph is nearly free (30x the nodes costs 1.2x the time); the pool is where the time goes.
/// `tags CONTAINSANY [...]` is a **full table scan of the lesson pool, every time** (an index was
/// measured and changed nothing; `EXPLAIN` reports `TableScan` on `surrealdb:v3.1.3`). The alternative
/// ADR-0091 sketched is `lesson -about-> interface` as a real edge: O(hits) instead of O(pool).
/// Asserts only that the two agree; the times are the finding.
#[test]
#[ignore = "writes hundreds of thousands of rows; run with --ignored --nocapture"]
fn what_an_edge_would_buy_over_a_tag_scan() {
    let Some(db) = Store::start() else {
        eprintln!("SKIPPED: could not start {SURREAL_IMAGE} — nothing was measured.");
        return;
    };

    let n = lessons();
    post_in_chunks(&db, &lesson_rows(n));
    let expected = n.div_ceil(TAGGED_EVERY);

    // The interface node, and one `about` edge per tagged lesson.
    db.last(&format!(
        "UPSERT interface:⟨{TAGGED}⟩ SET name = '{TAGGED}', exporter = '', consumers = 0, gen = 1;"
    ));
    let mut edges = String::new();
    for i in (0..n).step_by(TAGGED_EVERY) {
        edges.push_str(&format!(
            "RELATE memory:⟨stress-{i}⟩->about:⟨about-{i}⟩->interface:⟨{TAGGED}⟩ SET gen = 1;\n"
        ));
    }
    let edge_write = post_in_chunks(&db, &edges);

    let scan = format!("SELECT text FROM memory WHERE tags CONTAINSANY ['{TAGGED}'];");
    let walk = format!("SELECT VALUE <-about<-memory.text FROM ONLY interface:⟨{TAGGED}⟩;");

    let median = |q: &str| {
        let mut ts: Vec<Duration> = (0..5).map(|_| db.timed(q).0).collect();
        ts.sort();
        ts[2]
    };

    let scan_hits = db.last(&scan).as_array().map(|a| a.len()).unwrap_or(0);
    let walk_hits = db.last(&walk).as_array().map(|a| a.len()).unwrap_or(0);
    assert_eq!(scan_hits, expected, "the scan did not find the tagged lessons");
    assert_eq!(
        walk_hits, expected,
        "the edge traversal disagrees with the scan — a faster query that returns a \
         different answer is not an optimisation"
    );

    let (scan_ms, walk_ms) = (median(&scan), median(&walk));
    println!(
        "\n  pool {n} lessons, {expected} of them about {TAGGED}\n\
         \n    {:<28} {:>9}\n    {:<28} {:>9}\n    {:<28} {:>9}\n",
        "tag scan (CONTAINSANY)",
        format!("{:.1}ms", scan_ms.as_secs_f64() * 1000.0),
        "edge traversal (<-about<-)",
        format!("{:.1}ms", walk_ms.as_secs_f64() * 1000.0),
        "one-off cost to build edges",
        format!("{:.2}s", edge_write.as_secs_f64()),
    );
    println!(
        "  The scan is O(pool) and the traversal is O(hits), so this gap widens with \
         every run that ever finished. Neither is asserted — only that they agree."
    );
}
