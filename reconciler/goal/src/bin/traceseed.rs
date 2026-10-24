//! Seeds one run through `trace::Trace`, so the console's browser test also tests the writer.

use clap::Parser;
use comp_goal::trace::Trace;
use serde_json::json;

#[derive(Parser)]
#[command(name = "comp-trace-seed", about = "Write one run's history, for the console's e2e")]
struct Args {
    /// The SurrealDB HTTP endpoint.
    #[arg(long)]
    surreal_url: String,
    /// Matches `goalrun`'s default.
    #[arg(long, default_value = "goalmemory")]
    db: String,
    /// The password as a value (a throwaway test container); absent means unauthenticated.
    #[arg(long)]
    password: Option<String>,
}

fn main() {
    let args = Args::parse();
    let trace = Trace::new(&args.surreal_url, &args.db, args.password.as_deref());

    // The smallest run that still has something to explain.
    let run = "77/g1";
    trace.run_started(
        run,
        "add pagination to the search box",
        ".comp/goals/pagination.toml",
        77,
        "abc123",
        2,
    );

    // The miss is the row the run view calls out (ADR-0089).
    trace.capsearch(run, "paginate a result set", 1);
    trace.capsearch(run, "render a swimlane chart", 0);

    for (attempt, branch) in [("77/g1/risk-first", "risk-first"), ("77/g1/mvp", "mvp")] {
        trace.branch_spawned(run, attempt, branch, 1);
        trace.lesson_read(run, attempt, &["mem:paginate".to_string()]);
    }

    // Different approaches, or the run would demonstrate nothing about why a swarm is worth it.
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

    // Losers kept with their verdict: dropping them makes a run unexplainable.
    trace.gate_verdict(run, "77/g1/risk-first", 40, false, &json!({ "failing": ["test_pages"] }));
    // `tries` = 2: this branch needed a repair.
    trace.attempt_finished(run, "77/g1/risk-first", "failed", 40, &risk_files, 18_400, 94_000, 2);

    trace.gate_verdict(run, "77/g1/mvp", 100, true, &json!({ "failing": [] }));
    trace.attempt_finished(run, "77/g1/mvp", "passed", 100, &mvp_files, 31_200, 156_000, 1);

    trace.capability_added(run, "paginate", "components/paginate");

    trace.run_resolved(run, "pr-opened", Some("mvp"), "https://example.test/pull/1");

    // A half-written seed would make the browser test report a UI bug.
    if let Some(why) = trace.report() {
        eprintln!("comp-trace-seed: {why}");
        std::process::exit(1);
    }
    println!("{run}");
}
