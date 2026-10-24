//! A run's trace against a real database (ADR-0092); a rejected write fails quietly otherwise.

use std::time::Duration;

use comp_goal::trace::Trace;
use serde_json::Value;

// Shared harness, included rather than copied (docs/adr/0115).
#[path = "../../tests/harness/mod.rs"]
mod harness;
use harness::{Store, SURREAL_IMAGE};

/// Shares the graph's namespace so a run and its lessons join (ADR-0091).
const DB: &str = "goalmemory";

/// In the trace's own namespace/database, not the one `harness::Store` defines.
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

    let run = "77/g1";
    trace.run_started(
        run,
        "add pagination to the search box",
        ".comp/goals/pagination.toml",
        77,
        "abc123",
        2,
    );
    trace.capsearch(run, "paginate a result set", 1);
    trace.capsearch(run, "render a swimlane chart", 0);

    for (attempt, branch) in [("77/g1/risk-first", "risk-first"), ("77/g1/mvp", "mvp")] {
        trace.branch_spawned(run, attempt, branch, 1);
        trace.lesson_read(run, attempt, &["mem:paginate".to_string()]);
    }
    trace.gate_verdict(
        run,
        "77/g1/risk-first",
        40,
        false,
        &serde_json::json!({"failing": ["test_pages"]}),
    );
    trace.attempt_finished(
        run,
        "77/g1/risk-first",
        "failed",
        40,
        &serde_json::json!([{"path": "apps/search/a.rs"}]),
        18_400,
        94_000,
        1,
    );
    trace.gate_verdict(run, "77/g1/mvp", 100, true, &serde_json::json!({"failing": []}));
    trace.attempt_finished(
        run,
        "77/g1/mvp",
        "passed",
        100,
        &serde_json::json!([{"path": "components/paginate/src/lib.rs"}]),
        31_200,
        156_000,
        1,
    );
    trace.run_resolved(run, "pr-opened", Some("mvp"), "https://example.test/pull/1");

    assert!(
        trace.report().is_none(),
        "writes were dropped against a live database: {:?}",
        trace.report()
    );

    assert_eq!(count(&store, "run"), 1, "the run node");
    assert_eq!(count(&store, "attempt"), 2, "one node per branch");
    // 1 run-started + 2 capsearch + 2 branch-spawned + 2 lesson-read
    // + 2 gate-verdict + 2 attempt-finished + 1 run-resolved.
    assert_eq!(count(&store, "event"), 12, "one event per thing that happened");

    // On the run node, not reconstructed from events (ADR-0092).
    let r = rows(&store, "SELECT outcome, winner, url FROM run;");
    assert_eq!(r[0]["outcome"], "pr-opened");
    assert_eq!(r[0]["winner"], "mvp");

    // Fields: a timeline reconstructing bounds from events errs when one is missing.
    let bounds = rows(
        &store,
        "SELECT count() FROM run WHERE started_at != NONE AND resolved_at != NONE GROUP ALL;",
    );
    assert_eq!(bounds[0]["count"], 1, "the run carries its own bounds");

    // A miss signals what to build next (ADR-0089).
    let misses = rows(&store, "SELECT data.query AS q FROM event WHERE kind = 'capsearch-miss';");
    assert_eq!(misses[0]["q"], "render a swimlane chart");

    // The run is an attribute, not the key.
    trace.capability_added(run, "paginate", "components/paginate");
    let cap = rows(&store, "SELECT name, path, added_by FROM capability;");
    assert_eq!(cap[0]["name"], "paginate");
    assert_eq!(cap[0]["added_by"], run, "a capability must say which run taught it");

    // Paths and cost exist nowhere else once the terminal is gone.
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

    // A goal is prose a person typed: it must stay data.
    let nasty = r#"add a "search" box'; DELETE event; --"#;
    trace.run_started("78/g1", nasty, ".comp/goal.toml", 78, "def456", 1);
    assert!(trace.report().is_none(), "the quoted goal broke its own statement");
    // 12 from the run, +1 capability-added, +1 for the hostile run below.
    assert_eq!(count(&store, "event"), 14, "DELETE event ran as SurrealQL — the log is gone");
    let back = rows(&store, "SELECT goal FROM run WHERE id_text = '78/g1';");
    assert_eq!(back[0]["goal"], nasty, "the goal did not round-trip as written");
}

#[test]
fn a_dead_database_costs_a_line_of_output_and_nothing_else() {
    // Port 1 is not listening; a run must not die for lack of telemetry.
    let trace = Trace::new("http://127.0.0.1:1", DB, None);
    let started = std::time::Instant::now();

    trace.run_started("dead/g1", "a goal", ".comp/goal.toml", 1, "abc", 1);
    trace.branch_spawned("dead/g1", "dead/g1/a", "a", 1);
    trace.attempt_finished("dead/g1", "dead/g1/a", "errored", 0, &serde_json::json!([]), 0, 0, 0);
    trace.run_resolved("dead/g1", "failed", None, "");

    let report = trace.report().expect("dropped writes must be reported");
    assert!(
        report.contains("the run is unaffected"),
        "the report must say the run survived: {report}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "a dead database stalled the run for {:?} — the timeout is not bounding it",
        started.elapsed()
    );
}

/// `None` once defaulted to "root", whose Basic header an `--unauthenticated` server rejects.
/// Its own container: `--unauthenticated` is a server-start flag.
#[test]
fn an_unauthenticated_database_takes_writes_with_no_auth_header() {
    use std::process::{Command, Stdio};

    let port = comp_fleet::fleet::free_port();
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

    let trace = Trace::new(&url, DB, None);
    trace.run_started("unauth/g1", "a goal with no password", ".comp/goal.toml", 1, "abc", 1);
    trace.run_resolved("unauth/g1", "pr-opened", Some("only"), "");

    assert!(
        trace.report().is_none(),
        "writes were dropped against an unauthenticated database — a Basic header \
         naming a user it does not have is refused, so `None` must send none: {:?}",
        trace.report()
    );
}

/// Written with the real `run_id`, since a hand-typed id would pass after its format changed.
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

    let seed: u64 = 1_700_000_000;
    let run = seed.to_string();
    trace.run_started(&run, "a real goal", ".comp/goal.toml", seed, "deadbeef", 2);
    for round in 0..2 {
        for branch in ["risk-first", "mvp"] {
            let attempt = comp_goal::memory::run_id(seed, round, branch);
            trace.branch_spawned(&run, &attempt, branch, round);
            trace.attempt_finished(
                &run,
                &attempt,
                "failed",
                10,
                &serde_json::json!([]),
                100,
                1_000,
                1,
            );
        }
    }
    trace.run_resolved(&run, "exhausted", None, "");
    assert!(trace.report().is_none(), "writes dropped: {:?}", trace.report());

    // Verbatim from the console's `run_detail`.
    let attempts = rows(
        &store,
        // SurrealDB v3 requires the ordered field in the projection (a 400 otherwise).
        &format!(
            "SELECT id_text, started_at FROM attempt WHERE run = '{run}' ORDER BY started_at;"
        ),
    );
    assert_eq!(
        attempts.as_array().map(|a| a.len()),
        Some(4),
        "the run and its attempts did not join on goalrun's ids: {attempts:?}"
    );
    // The ids a person reads in the graph: `seed/g1/branch`.
    let first = attempts[0]["id_text"].as_str().unwrap_or_default().to_string();
    assert!(
        first.starts_with(&format!("{seed}/g")),
        "an attempt id stopped being legible: {first}"
    );
}
