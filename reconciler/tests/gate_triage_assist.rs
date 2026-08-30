//! The two portable `triage:assist` gates, ported from
//! `components/triage-assist-domain/e2e-*.sh`. `e2e-assist.sh` wants a model on :8787
//! and stays a shell gate.
//!
//! `e2e-intake.sh` is also the gate that went flaky in CI — "the component never
//! served /health" once, passing on a rerun, with the cause invisible because the
//! shell reports only `tail -3` of the host log and got three lines of stack trace.
//! A Rust gate that fails prints the assertion that failed.

mod gatelib;
use gatelib::{field, Gate};
use serde_json::{json, Value};

const CRATE: &str = "triage-assist-domain";

fn start(config: &[&str]) -> Option<Gate> {
    Gate::compose_and_start("triage-assist", CRATE, config)
}
fn parse(t: &str) -> Value {
    serde_json::from_str(t.trim()).unwrap_or(Value::Null)
}
fn token(gate: &Gate, subject: &str, scopes: Option<Value>) -> String {
    let mut b = json!({ "subject": subject });
    if let Some(s) = scopes {
        b["scopes"] = s;
    }
    let t = field(&gate.post("/test/token", None, b).1, "token");
    assert!(
        !t.is_empty(),
        "POST /test/token returned no token — the scaffold is broken, not the part"
    );
    t
}

#[test]
fn intake_masks_records_the_reporter_and_limits_per_subject() {
    // `max-attempts=3` is set by the gate, not the app.
    let Some(gate) = start(&["max-attempts=3", "lockout-window=60"]) else { return };
    let report = json!({"title":"Search returns nothing","body":"contact me at ada@example.test","component":"search"});

    // --- the three refusals, none of which spends an attempt -----------------------
    let (c, _) = gate.post("/api/reports", None, report.clone());
    assert_eq!(c, 401, "a report with no bearer token must be 401 unauthenticated");
    let ro = token(&gate, "reader", Some(json!(["reports:read"])));
    let (c, _) = gate.post("/api/reports", Some(&ro), report.clone());
    assert_eq!(
        c, 403,
        "a token with only reports:read must be 403 forbidden — 401 says 'log in' to a caller who is logged in"
    );
    let writer = token(&gate, "ada", None);
    let (c, _) =
        gate.post("/api/reports", Some(&writer), json!({"title":"","body":"x","component":"web"}));
    assert_eq!(c, 400, "an empty title must be 400 invalid_report");

    // --- a report goes in, and what is stored is masked ---------------------------
    let (_, resp) = gate.post("/api/reports", Some(&writer), report);
    let id = field(&resp, "id");
    assert!(!id.is_empty(), "POST /api/reports returned no id: {resp}");

    let stored = gate.stored("report", &id);
    assert!(
        !stored.contains("ada@example.test"),
        "the reporter's email was stored verbatim — it must be masked: {stored}"
    );
    assert!(
        stored.contains("[EMAIL]"),
        "the body was not masked with pii:redact's placeholder: {stored}"
    );
    let d = parse(&stored);
    assert_eq!(d["state"], "open", "a new report must be open: {d}");
    assert_eq!(
        d["reporter"], "ada",
        "reporter must be the principal's subject, not {:?}",
        d["reporter"]
    );
    assert_eq!(d["component"], "search", "{d}");
    assert!(
        d.get("assist").is_none(),
        "intake must not invent an assist — that is the assist part's job"
    );
    assert!(
        d["reported_at"].as_str().unwrap_or_default().ends_with('Z'),
        "reported_at must be RFC3339 UTC: {:?}",
        d["reported_at"]
    );

    // --- reading it back, through the part's own route ---------------------------
    let (_, read) = gate.get(&format!("/api/reports/{id}"), Some(&writer));
    assert_eq!(
        parse(&read)["title"],
        "Search returns nothing",
        "GET /api/reports/{{id}} did not answer the stored report: {read}"
    );
    let (c, _) = gate.get("/api/reports/nope", Some(&writer));
    assert_eq!(c, 404, "an unknown report id must be 404");
    let (c, _) = gate.get(&format!("/api/reports/{id}"), None);
    assert_eq!(c, 401, "reading a report with no bearer must be 401");

    // --- the filter, which is an index lookup and not a scan ----------------------
    let (_, list) = gate.get("/api/reports?component=search", Some(&writer));
    let parsed = parse(&list);
    let ids: Vec<&str> = parsed["reports"]
        .as_array()
        .map(|a| a.iter().filter_map(|r| r["id"].as_str()).collect())
        .unwrap_or_default();
    assert!(ids.contains(&id.as_str()), "filtering on component=search missed it: {list}");
    let (_, none) = gate.get("/api/reports?component=nothing-files-bugs-here", Some(&writer));
    assert_eq!(
        parse(&none)["reports"],
        json!([]),
        "a filter matching nothing must answer an empty list, not everything"
    );

    // --- and the limit, which counts what was accepted ---------------------------
    //
    // A subject of its own: the key is the principal's subject, so counting `burst`
    // cannot be disturbed by what `ada` did above — and if a part keys the limiter on
    // something else (a path, a tenant, nothing at all), this is where that shows.
    let burst = token(&gate, "burst", None);
    for i in 1..=3 {
        let (c, _) = gate.post(
            "/api/reports",
            Some(&burst),
            json!({"title": format!("burst {i}"), "body":"b", "component":"web"}),
        );
        assert_eq!(c, 201, "report {i} of 3 within the limit must be accepted");
    }
    let (_, locked) = gate.post(
        "/api/reports",
        Some(&burst),
        json!({"title":"burst 4","body":"b","component":"web"}),
    );
    let (c, _) = gate.post(
        "/api/reports",
        Some(&burst),
        json!({"title":"burst 5","body":"b","component":"web"}),
    );
    assert_eq!(c, 429, "the 4th report from one subject in the window must be 429 rate_limited (the limit is max-attempts=3)");
    let d = parse(&locked);
    assert_eq!(d["error"], "rate_limited", "a 429 must tell the caller how long to wait: {d}");
    assert!(
        d["retry_after"].as_i64().unwrap_or(0) > 0,
        "retry_after must be the seconds the limiter reported: {d}"
    );

    // The other subject is unaffected — a limiter keyed on the wrong thing locks
    // everyone out at once, and a gate that only ever used one subject would call that
    // a pass.
    let (c, _) = gate.post(
        "/api/reports",
        Some(&writer),
        json!({"title":"still fine","body":"b","component":"web"}),
    );
    assert_eq!(c, 201, "locking out one subject must not lock out another");
}

#[test]
fn the_audit_ledger_is_by_trace_capped_and_not_public() {
    let Some(gate) = start(&[]) else { return };
    let t = token(&gate, "ada", None);

    // Two traces: one to write under, one to ask under. Asking under the trace being
    // asked about would make the answer include the question, and the count race that
    // follows would be nobody's fault.
    const TRACE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa1";
    const OTHER: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb2";

    for i in 1..=3 {
        gate.with_headers(
            "GET",
            &format!("/api/reports/r{i}"),
            Some(&t),
            &[("traceparent", &format!("00-{TRACE}-0000000000000001-01"))],
            None,
        );
    }

    let (_, raw) = gate.with_headers(
        "GET",
        &format!("/api/audit?trace={TRACE}"),
        Some(&t),
        &[("traceparent", &format!("00-{OTHER}-0000000000000002-01"))],
        None,
    );
    assert!(
        !raw.trim().is_empty(),
        "the route answered an empty body — it is not implemented, or it trapped"
    );
    let d = parse(&raw);
    let evs = d["events"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("the answer has no events list: {d}"));
    assert_eq!(
        evs.len(),
        3,
        "three requests were made under {TRACE}, the ledger has {}: {evs:?}",
        evs.len()
    );
    for e in &evs {
        assert_eq!(e["trace_id"], TRACE, "an event from another trace came back: {e}");
        assert_eq!(
            e["event"], "http.request",
            "the router notes dispatched requests as http.request: {e}"
        );
        assert_eq!(e["subject"], "router", "the router's own events are subject 'router': {e}");
        assert_eq!(e["tenant"], "triage-assist", "tenant must be the app's: {e}");
        assert!(
            e["id"].as_str().is_some_and(|s| !s.is_empty()),
            "an event with no id cannot be referred to: {e}"
        );
        assert!(e["timestamp"].as_i64().unwrap_or(0) > 0, "timestamp must be unix seconds: {e}");
        assert!(
            e["detail"].as_str().is_some_and(|s| !s.is_empty()),
            "an event with no detail says nothing an operator can use: {e}"
        );
    }

    // A trace nobody used is an empty list, not everything. `by-trace` returning the
    // whole log looks like a working filter until the first real incident.
    let unused = "c".repeat(32);
    let (_, raw) = gate.get(&format!("/api/audit?trace={unused}"), Some(&t));
    assert!(
        !raw.trim().is_empty(),
        "the route answered an empty body — it is not implemented, or it trapped"
    );
    assert_eq!(
        parse(&raw)["events"],
        json!([]),
        "an unused trace answered {} events",
        parse(&raw)["events"].as_array().map(|a| a.len()).unwrap_or(0)
    );

    // The limit, and the cap on it.
    let evs = |q: &str| -> Vec<Value> {
        parse(&gate.get(q, Some(&t)).1)["events"].as_array().cloned().unwrap_or_default()
    };
    let two = evs("/api/audit?limit=2");
    assert_eq!(two.len(), 2, "?limit=2 answered {}", two.len());
    let ts: Vec<i64> = two.iter().filter_map(|e| e["timestamp"].as_i64()).collect();
    let mut desc = ts.clone();
    desc.sort_by(|a, b| b.cmp(a));
    assert_eq!(ts, desc, "newest first, and these are not: {ts:?}");
    assert!(evs("/api/audit?limit=500").len() <= 100, "the limit is capped at 100");
    let d = evs("/api/audit");
    assert!(!d.is_empty() && d.len() <= 20, "no limit given means 20, this answered {}", d.len());

    // The trail is not public, and reading it is a read.
    let (c, _) = gate.get("/api/audit", None);
    assert_eq!(c, 401, "reading the audit trail with no bearer must be 401");
    let wo = token(&gate, "nosy", Some(json!(["reports:write"])));
    let (c, _) = gate.get("/api/audit", Some(&wo));
    assert_eq!(c, 403, "a token that may write but not read must be 403 on the audit trail");
}
