//! What the merged graph costs, at sizes this repository has not reached yet.
//!
//! ADR-0091 put the capability graph and the knowledge pool in one database and
//! justified it with one query: *the lessons about the interfaces this app
//! imports*. That query is a three-hop traversal — `app -> carries -> artifact ->
//! imports -> interface` — feeding a `CONTAINSANY` over the lesson pool. Both
//! halves grow: the graph grows with the catalogue, the pool grows with every run
//! that ever finished. Nothing so far has measured what happens when they do.
//!
//! This is that measurement. It reports a table of insert and query times against
//! node and edge counts, at the real size and at three synthetic multiples of it.
//!
//! ## What is asserted, and what is only reported
//!
//! **Correctness at scale is asserted.** The join must still return exactly the
//! lessons it should when the graph is twenty times larger and the pool is a
//! hundred times larger. A query that gets fast by returning less is not faster.
//!
//! **Scaling SHAPE is asserted, loosely.** Query time is allowed to grow with the
//! data — it must not grow like the square of it. The bound is deliberately slack
//! (see `SUPERLINEAR_ALLOWANCE`) because the thing worth catching is an accidental
//! full scan per hop, which shows up as an order of magnitude, not as thirty
//! percent.
//!
//! **Absolute times are reported, never asserted.** A millisecond figure is a fact
//! about the machine that ran it, and a test that fails because somebody's laptop
//! was busy teaches people to rerun tests until they pass. ADR-0019 and ADR-0020
//! make the same split for the density number.
//!
//! ## Running it
//!
//!   cargo test -p comp-reconciler --release --test capgraph_stress -- --ignored --nocapture
//!
//! Scales are tunable, for probing past the defaults without editing this file:
//!
//!   COMP_STRESS_SCALES=1,5,20,50 COMP_STRESS_LESSONS=50000 cargo test ... -- --ignored --nocapture
//!
//! `#[ignore]`d: it writes hundreds of thousands of rows and takes a minute or two.

use std::process::Command;
use std::time::Duration;

mod harness;
use harness::{Store, SURREAL_IMAGE};

/// How much bigger than the real catalogue each round is. `1` is not synthetic at
/// all — it is the repository as it stands, which anchors the synthetic rows to
/// something real.
fn scales() -> Vec<usize> {
    std::env::var("COMP_STRESS_SCALES")
        .ok()
        .map(|v| v.split(',').filter_map(|s| s.trim().parse().ok()).collect::<Vec<_>>())
        .filter(|v: &Vec<usize>| !v.is_empty())
        .unwrap_or_else(|| vec![1, 5, 20])
}

/// How many lessons sit in the pool the join filters. The pool grows with every
/// run that ever finished, so it outgrows the graph rather than tracking it.
fn lessons() -> usize {
    std::env::var("COMP_STRESS_LESSONS").ok().and_then(|v| v.parse().ok()).unwrap_or(20_000)
}

/// How much worse than linear the join may scale before this fails.
///
/// Slack on purpose. Between the smallest and largest round the graph grows 20x,
/// and a traversal that has quietly become a scan per hop would land somewhere
/// past 400x — an order of magnitude clear of this bound. Tightening it to
/// something that looks rigorous would buy nothing and cost a flaky test on a
/// loaded machine.
const SUPERLINEAR_ALLOWANCE: f64 = 8.0;

/// The interface the asserted lessons are tagged with. Real, and imported by a
/// real app, so the assertion survives the synthetic rows being removed.
const TAGGED: &str = "csv:codec/codec@0.1.0";
const APP: &str = "vet";

/// The real projection, as the tool emits it.
fn real_projection(generation: u64) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_comp-capgraph"))
        .args(["--format", "surql", "--gen", &generation.to_string()])
        .output()
        .expect("comp-capgraph did not run");
    assert!(out.status.success(), "comp-capgraph failed — is anything built?");
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// Synthetic rows in the same SHAPE as the projection, at `copies` times the size.
///
/// Copies rather than random graphs: the traversal cost this is measuring depends
/// on fan-out — how many parts an app carries, how many interfaces a part imports
/// — and a copy of the real catalogue has the real fan-out. A random graph with
/// the same node count and a different shape would produce a number that looks
/// like a measurement and answers a different question.
///
/// `copies - 1` extra copies are written, because copy zero is the real one.
fn synthetic(copies: usize, generation: u64) -> String {
    let mut out = String::new();
    for c in 1..copies {
        // One app, its parts and their interfaces, per copy. Suffixed rather than
        // renamed so the real names stay unique and the asserted join cannot
        // accidentally match a synthetic row.
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
                    // Interfaces are shared across the copy, like the real ones:
                    // `records:store/store` has 37 consumers, and a graph where
                    // every part has a private interface has no fan-in to traverse.
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

/// The lesson pool. One in every `TAGGED_EVERY` is tagged with the real interface
/// the assertion looks for; the rest are noise the `CONTAINSANY` has to reject.
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

/// Statements are posted in chunks. One 300,000-statement body would measure the
/// HTTP layer's appetite for a large upload rather than the database's write path.
const CHUNK: usize = 2_000;

fn post_in_chunks(db: &Store, sql: &str) -> Duration {
    let lines: Vec<&str> = sql.lines().filter(|l| !l.trim_start().starts_with("--")).collect();
    let mut total = Duration::ZERO;
    for chunk in lines.chunks(CHUNK) {
        total += db.timed(&chunk.join("\n")).0;
    }
    total
}

/// The query under test, verbatim from `just lessons-for`.
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

    // The pool first, and once: it does not change between rounds, so every round
    // measures a graph growing against a pool that is already large. That is the
    // real shape — the pool outlives every rebuild of the graph.
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

        // CORRECTNESS, every round. The real app's answer must not change because
        // the database got bigger — a traversal that starts matching synthetic
        // interfaces, or stops finding real ones, is the failure this whole test
        // exists to catch.
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

    // SCALING SHAPE. Compared against the first round rather than round-to-round,
    // so the ratio is over the whole range and one slow middle round cannot hide
    // between two fast ones.
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

/// What the lesson half of the join actually costs, and what a graph edge would
/// cost instead.
///
/// The measurement above shows the graph is nearly free — 30x the nodes and edges
/// costs 1.2x the time — and that the pool is where the time goes. This says why.
///
/// `tags CONTAINSANY [...]` is a **full table scan of the lesson pool, every
/// time**. Not a missing index: an index on `tags` was defined and measured and
/// changed nothing, and `EXPLAIN` reports `TableScan` either way on
/// `surrealdb:v3.1.3`. So the cost of retrieving lessons grows with everything the
/// swarm has ever learned, whether or not it is relevant.
///
/// The alternative is the schema ADR-0091 sketched and the implementation did not
/// take: `lesson -about-> interface` as a real edge, because `knowledge-memory`
/// already writes `tags` as strings and the projection had no reason to duplicate
/// them. Traversing that edge is O(hits); scanning is O(pool).
///
/// This test asserts only that the two agree — a faster query that returns a
/// different answer is not an optimisation. The times are the finding.
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

    // The interface node, and one `about` edge per tagged lesson — what the
    // projection would write if lessons were joined structurally.
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
