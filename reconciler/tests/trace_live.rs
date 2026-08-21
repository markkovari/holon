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
    trace.attempt_finished(run, "77/g1/risk-first", "failed", 40, &serde_json::json!([{"path": "apps/search/a.rs"}]), 18_400, 94_000, 1);
    trace.gate_verdict(run, "77/g1/mvp", 100, true, &serde_json::json!({"failing": []}));
    trace.attempt_finished(run, "77/g1/mvp", "passed", 100, &serde_json::json!([{"path": "components/paginate/src/lib.rs"}]), 31_200, 156_000, 1);
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

    // What the pool gained (ADR-0089), and which run taught it. A capability node
    // rather than only an event, because "what can this system do" is a question
    // about the capability — the run that added it is an attribute, not the key.
    trace.capability_added(run, "paginate", "components/paginate");
    let cap = rows(&store, "SELECT name, path, added_by FROM capability;");
    assert_eq!(cap[0]["name"], "paginate");
    assert_eq!(cap[0]["added_by"], run, "a capability must say which run taught it");

    // The branch's paths are on the attempt, and its cost with them. Both exist
    // nowhere else once the terminal is gone — the diff is in the pull request,
    // but what a branch SPENT is not.
    let won = rows(
        &store,
        "SELECT paths, files, tokens, elapsed_ms FROM attempt WHERE id_text = '77/g1/mvp';",
    );
    assert_eq!(won[0]["files"], 1, "the file count did not land");
    assert_eq!(won[0]["tokens"], 31_200);
    assert!(
        won[0]["paths"][0].as_str().unwrap_or_default().starts_with("components/"),
        "the winning branch's paths were lost: {:?}",
        won[0]["paths"]
    );

    // 3. A goal that contains quotes and SurrealQL is DATA, not syntax.
    let nasty = r#"add a "search" box'; DELETE event; --"#;
    trace.run_started("78/g1", nasty, 78, "def456", 1);
    assert!(trace.report().is_none(), "the quoted goal broke its own statement");
    // 12 from the run, +1 capability-added, +1 for the hostile run below.
    assert_eq!(count(&store, "event"), 14, "DELETE event ran as SurrealQL — the log is gone");
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
    trace.attempt_finished("dead/g1", "dead/g1/a", "errored", 0, &serde_json::json!([]), 0, 0, 0);
    trace.run_resolved("dead/g1", "failed", None, "");

    let report = trace.report().expect("dropped writes must be reported");
    assert!(report.contains("the run is unaffected"), "the report must say the run survived: {report}");
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "a dead database stalled the run for {:?} — the timeout is not bounding it",
        started.elapsed()
    );
}

/// An unauthenticated database takes writes with NO auth header.
///
/// This is the path `comp-trace-seed` and any local `goalrun` without
/// `--surreal-password` take, and it was broken while every test passed: `None`
/// was defaulted to `"root"`, so a Basic header naming a user the server does
/// not have went out — and an `--unauthenticated` SurrealDB rejects that with a
/// non-JSON body. Every write was dropped, silently, exactly as designed.
///
/// Its own container, because `--unauthenticated` is a server-start flag and the
/// shared harness starts an authenticated one.
#[test]
fn an_unauthenticated_database_takes_writes_with_no_auth_header() {
    use std::process::{Command, Stdio};

    let port = comp_reconciler::fleet::free_port();
    let name = format!("trace-unauth-{port}");
    let started = Command::new("docker")
        .args(["run", "--rm", "-d", "--name", &name])
        .args(["-p", &format!("127.0.0.1:{port}:8000")])
        .arg(SURREAL_IMAGE)
        .args(["start", "--no-banner", "--unauthenticated"])
        .args(["--bind", "0.0.0.0:8000", "memory"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !started {
        eprintln!("SKIPPED: docker could not start {SURREAL_IMAGE} unauthenticated");
        return;
    }
    struct Container(String);
    impl Drop for Container {
        fn drop(&mut self) {
            let _ = Command::new("docker")
                .args(["rm", "-f", &self.0])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
    let _c = Container(name);

    let url = format!("http://127.0.0.1:{port}");
    let http = reqwest::blocking::Client::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while std::time::Instant::now() < deadline {
        if http.get(format!("{url}/health")).send().is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }

    // No password: the case the seeder and a local run actually use.
    let trace = Trace::new(&url, DB, None);
    trace.run_started("unauth/g1", "a goal with no password", 1, "abc", 1);
    trace.run_resolved("unauth/g1", "merged", Some("only"), "");

    assert!(
        trace.report().is_none(),
        "writes were dropped against an unauthenticated database — a Basic header \
         naming a user it does not have is refused, so `None` must send none: {:?}",
        trace.report()
    );
}

/// `goalrun`'s own id scheme joins run to attempts.
///
/// The driver uses the seed as the run id and `memory::run_id(seed, round,
/// branch)` — the function that already existed, for attributing verdicts — as
/// the attempt id. Those ids contain `/`, and the console finds a run's attempts
/// with `WHERE run = <id>`, so the two have to agree exactly.
///
/// Written with the REAL function rather than a hand-typed string: a test that
/// spelled the id itself would keep passing if `run_id`'s format changed, which
/// is the one way this join can silently break.
#[test]
fn goalruns_own_ids_join_a_run_to_its_attempts() {
    let Some(store) = Store::start() else {
        eprintln!("SKIPPED: docker could not start {SURREAL_IMAGE}");
        return;
    };
    let trace = Trace::new(
        &format!("http://127.0.0.1:{}", store.port()),
        DB,
        Some(harness::SURREAL_PASSWORD),
    );

    // Exactly what `comp-goalrun` does: the seed is the run, `run_id` is the
    // attempt, two branches over two rounds.
    let seed: u64 = 1_700_000_000;
    let run = seed.to_string();
    trace.run_started(&run, "a real goal", seed, "deadbeef", 2);
    for round in 0..2 {
        for branch in ["risk-first", "mvp"] {
            let attempt = comp_reconciler::memory::run_id(seed, round, branch);
            trace.branch_spawned(&run, &attempt, branch, round);
            trace.attempt_finished(&run, &attempt, "failed", 10, &serde_json::json!([]), 100, 1_000, 1);
        }
    }
    trace.run_resolved(&run, "exhausted", None, "");
    assert!(trace.report().is_none(), "writes dropped: {:?}", trace.report());

    // The console's own query, verbatim from `run_detail`.
    let attempts = rows(
        &store,
        // `started_at` is in the projection because SurrealDB v3 REQUIRES the
        // ordered field to be selected — omitting it is a 400, not a silent
        // reorder. The console's own queries are safe only because they either
        // `SELECT *` or happen to list the field they order by.
        &format!("SELECT id_text, started_at FROM attempt WHERE run = '{run}' ORDER BY started_at;"),
    );
    assert_eq!(
        attempts.as_array().map(|a| a.len()),
        Some(4),
        "the run and its attempts did not join on goalrun's ids: {attempts:?}"
    );
    // And the ids are the ones a person reads in the graph — `seed/g1/branch`,
    // not an opaque number (the reason `run_id` is shaped that way).
    let first = attempts[0]["id_text"].as_str().unwrap_or_default().to_string();
    assert!(
        first.starts_with(&format!("{seed}/g")),
        "an attempt id stopped being legible: {first}"
    );
}
