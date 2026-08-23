//! `comp-trace-seed` — write one run's history, for a console the browser can drive.
//!
//! The console's Playwright suite needs a run to look at. This writes one through
//! `trace::Trace` — the SAME code path `comp-goalrun` uses (ADR-0092) — rather
//! than inserting rows that happen to match what the UI renders.
//!
//! That distinction is the whole point. A hand-written fixture keeps passing
//! after the writer's schema drifts, so the e2e would go green while the real run
//! view showed nothing. Seeding through the writer means the browser test is
//! also a test of what the driver records.
//!
//!   comp-trace-seed --surreal-url http://127.0.0.1:8000 [--db goalmemory] [--password root]
//!
//! Deliberately not `#[cfg(test)]` machinery: the Playwright harness is a Node
//! process and cannot call into a Rust test, so this is a real binary.

use clap::Parser;
use comp_reconciler::trace::Trace;
use serde_json::json;

#[derive(Parser)]
#[command(name = "comp-trace-seed", about = "Write one run's history, for the console's e2e")]
struct Args {
    /// The SurrealDB HTTP endpoint — the same one the graph component is given.
    #[arg(long)]
    surreal_url: String,
    /// The database. Matches `goalrun`'s default so the console reads one place.
    #[arg(long, default_value = "goalmemory")]
    db: String,
    /// The password, as a VALUE — and ABSENT means unauthenticated, exactly as
    /// `goalrun --surreal-password` does.
    ///
    /// No default. A default of "root" would send a Basic header naming a user an
    /// `--unauthenticated` server does not have, which it refuses — so the
    /// convenience default would break the only setup this seeder is for. That
    /// bug shipped once already, one layer down, for the same reason.
    ///
    /// A value in argv is acceptable here and not in `goalrun` (which takes a
    /// file path): this seeds a throwaway container for a browser test.
    #[arg(long)]
    password: Option<String>,
}

fn main() {
    let args = Args::parse();
    let trace = Trace::new(&args.surreal_url, &args.db, args.password.as_deref());

    // One generation, two branches, one winner — the smallest run that still has
    // something to explain. A single-branch run would render fine and prove
    // nothing about "why did this one beat that one".
    let run = "77/g1";
    trace.run_started(run, "add pagination to the search box", ".comp/goals/pagination.toml", 77, "abc123", 2);

    // A hit and a MISS. The miss is the row the run view calls out, because it
    // is the graph naming a capability the pool lacks (ADR-0089).
    trace.capsearch(run, "paginate a result set", 1);
    trace.capsearch(run, "render a swimlane chart", 0);

    for (attempt, branch) in [("77/g1/risk-first", "risk-first"), ("77/g1/mvp", "mvp")] {
        trace.branch_spawned(run, attempt, branch, 1);
        trace.lesson_read(run, attempt, &["mem:paginate".to_string()]);
    }

    // The two branches took DIFFERENT approaches, which is the whole reason to
    // run more than one: risk-first edited the app in place and failed its tests;
    // mvp extracted a reusable component and passed. A seeded run where both
    // branches wrote the same files would render correctly and demonstrate
    // nothing about why a swarm is worth its cost.
    let risk_files = json!([
        { "path": "apps/search/src/handler.rs", "content": "" },
        { "path": "apps/search/src/query.rs", "content": "" },
    ]);
    let mvp_files = json!([
        { "path": "components/paginate/wit/paginate.wit", "content": "" },
        { "path": "components/paginate/src/lib.rs", "content": "" },
        { "path": "components/paginate/Cargo.toml", "content": "" },
        { "path": "apps/search/src/handler.rs", "content": "" },
    ]);

    // The loser is kept, with its verdict. Dropping failed branches is what makes
    // a run unexplainable a day later.
    trace.gate_verdict(run, "77/g1/risk-first", 40, false, &json!({ "failing": ["test_pages"] }));
    // The last argument is `tries`: this branch needed a repair, the one below did
    // not, and the seeded run is the console's fixture for showing that difference.
    trace.attempt_finished(run, "77/g1/risk-first", "failed", 40, &risk_files, 18_400, 94_000, 2);

    trace.gate_verdict(run, "77/g1/mvp", 100, true, &json!({ "failing": [] }));
    trace.attempt_finished(run, "77/g1/mvp", "passed", 100, &mvp_files, 31_200, 156_000, 1);

    // What the pool gained. The point of ADR-0089: the app change lands and is
    // done, the component is there for every run after this one.
    trace.capability_added(run, "paginate", "components/paginate");

    trace.run_resolved(run, "merged", Some("mvp"), "https://example.test/pull/1");

    // A seeder that half-wrote is worse than one that failed: the browser test
    // would then assert against an incomplete run and report a UI bug.
    if let Some(why) = trace.report() {
        eprintln!("comp-trace-seed: {why}");
        std::process::exit(1);
    }
    println!("{run}");
}
