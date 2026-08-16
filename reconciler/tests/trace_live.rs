//! A run's trace, against a real database (ADR-0092).
//!
//! `trace.rs`'s unit tests check the SurrealQL it *builds*. This checks what the
//! database *answers* — the half this repository has repeatedly got wrong
//! (ADR-0061, ADR-0080) and the half that matters, because a trace writer whose
//! statements are rejected fails exactly the way it is designed not to: quietly,
//! counting drops, while the run carries on.
//!
//! The claims:
//!
//!   1. A whole run's history lands — run and attempt nodes, and one event per
//!      thing that happened.
//!   2. **A dropped write never fails a run.** Pointed at a dead address, every
//!      call still returns, and the count says so.
//!   3. A goal containing quotes and SurrealQL survives as *data*. This is the
//!      one that would be a security bug rather than a missing feature: a goal
//!      is prose a person typed.
//!
//! Skipped, loudly, when Docker cannot start the database.

use std::time::Duration;

use comp_reconciler::trace::Trace;
use serde_json::Value;

mod harness;
use harness::{Store, SURREAL_IMAGE};

/// The database `goalrun` points the graph at. The trace shares the namespace so
/// a run and the lessons it produced can be joined (ADR-0091).
const DB: &str = "goalmemory";

/// Count rows in `table` within the trace's own namespace/database, which is not
/// the one `harness::Store` defines — so this asks over its own request.
fn count(store: &Store, table: &str) -> u64 {
    let body = format!("SELECT count() FROM {table} GROUP ALL;");
    let text = reqwest::blocking::Client::new()
        .post(format!("http://127.0.0.1:{}/sql", store.port()))
        .basic_auth("root", Some(harness::SURREAL_PASSWORD))
        .header("accept", "application/json")
        .header("surreal-ns", "comp")
        .header("surreal-db", DB)
        .body(body)
        .send()
        .and_then(|r| r.text())
        .unwrap_or_default();
    serde_json::from_str::<Vec<Value>>(&text)
        .ok()
        .and_then(|v| v.first().and_then(|s| s["result"][0]["count"].as_u64()))
        .unwrap_or(0)
}

fn rows(store: &Store, surql: &str) -> Value {
    let text = reqwest::blocking::Client::new()
        .post(format!("http://127.0.0.1:{}/sql", store.port()))
        .basic_auth("root", Some(harness::SURREAL_PASSWORD))
        .header("accept", "application/json")
        .header("surreal-ns", "comp")
        .header("surreal-db", DB)
        .body(surql.to_string())
        .send()
        .and_then(|r| r.text())
        .unwrap_or_default();
    serde_json::from_str::<Vec<Value>>(&text)
        .ok()
        .and_then(|v| v.last().map(|s| s["result"].clone()))
        .unwrap_or(Value::Null)
}

#[test]
fn a_whole_run_lands_and_a_dead_database_never_fails_one() {
    let Some(store) = Store::start() else {
        eprintln!("SKIPPED: docker could not start {SURREAL_IMAGE} — the trace is unverified");
        return;
    };

    let url = format!("http://127.0.0.1:{}", store.port());
    let trace = Trace::new(&url, DB, Some(harness::SURREAL_PASSWORD));

    // A run as it actually happens: two branches, one repair, one winner.
    let run = "77/g1";
    trace.run_started(run, "add pagination to the search box", 77, "abc123", 2);
    trace.capsearch(run, "paginate a result set", 1);
    trace.capsearch(run, "render a swimlane chart", 0);

    for (attempt, branch) in [("77/g1/risk-first", "risk-first"), ("77/g1/mvp", "mvp")] {
        trace.branch_spawned(run, attempt, branch, 1);
        trace.lesson_read(run, attempt, &["mem:paginate".to_string()]);
    }
    trace.gate_verdict(run, "77/g1/risk-first", 40, false, &serde_json::json!({"failing": ["test_pages"]}));
    trace.attempt_finished(run, "77/g1/risk-first", "failed", 40);
    trace.gate_verdict(run, "77/g1/mvp", 100, true, &serde_json::json!({"failing": []}));
    trace.attempt_finished(run, "77/g1/mvp", "passed", 100);
    trace.run_resolved(run, "merged", Some("mvp"), "https://example.test/pull/1");

    assert!(trace.report().is_none(), "writes were dropped against a live database: {:?}", trace.report());

    // 1. The history is there.
    assert_eq!(count(&store, "run"), 1, "the run node");
    assert_eq!(count(&store, "attempt"), 2, "one node per branch");
    // 1 run-started + 2 capsearch + 2 branch-spawned + 2 lesson-read
    // + 2 gate-verdict + 2 attempt-finished + 1 run-resolved.
    assert_eq!(count(&store, "event"), 12, "one event per thing that happened");

    // The winner is on the run node, not reconstructed by scanning events —
    // ADR-0092 puts run-level facts on the run.
    let r = rows(&store, "SELECT outcome, winner, url FROM run;");
    assert_eq!(r[0]["outcome"], "merged");
    assert_eq!(r[0]["winner"], "mvp");

    // `started_at` and `resolved_at` are FIELDS. A timeline that reconstructs its
    // own bounds from events gets them wrong the moment one is missing.
    let bounds = rows(&store, "SELECT count() FROM run WHERE started_at != NONE AND resolved_at != NONE GROUP ALL;");
    assert_eq!(bounds[0]["count"], 1, "the run carries its own bounds");

    // A capsearch MISS is retrievable on its own — it is the signal for what to
    // build next (ADR-0089), and it is worthless if it cannot be found.
    let misses = rows(&store, "SELECT data.query AS q FROM event WHERE kind = 'capsearch-miss';");
    assert_eq!(misses[0]["q"], "render a swimlane chart");

    // 3. A goal that contains quotes and SurrealQL is DATA, not syntax.
    let nasty = r#"add a "search" box'; DELETE event; --"#;
    trace.run_started("78/g1", nasty, 78, "def456", 1);
    assert!(trace.report().is_none(), "the quoted goal broke its own statement");
    assert_eq!(count(&store, "event"), 13, "DELETE event ran as SurrealQL — the log is gone");
    let back = rows(&store, "SELECT goal FROM run WHERE id_text = '78/g1';");
    assert_eq!(back[0]["goal"], nasty, "the goal did not round-trip as written");
}

#[test]
fn a_dead_database_costs_a_line_of_output_and_nothing_else() {
    // Port 1 is not listening. Every call must still return, because a run that
    // dies because its telemetry could not be written is worse than a run with
    // no telemetry.
    let trace = Trace::new("http://127.0.0.1:1", DB, None);
    let started = std::time::Instant::now();

    trace.run_started("dead/g1", "a goal", 1, "abc", 1);
    trace.branch_spawned("dead/g1", "dead/g1/a", "a", 1);
    trace.attempt_finished("dead/g1", "dead/g1/a", "errored", 0);
    trace.run_resolved("dead/g1", "failed", None, "");

    let report = trace.report().expect("dropped writes must be reported");
    assert!(report.contains("the run is unaffected"), "the report must say the run survived: {report}");
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "a dead database stalled the run for {:?} — the timeout is not bounding it",
        started.elapsed()
    );
}
