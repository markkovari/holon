//! The three `triage` gates, ported from `components/triage-domain/e2e-*.sh`.
//!
//! Assertions and failure sentences unchanged — ADR-0088 makes a gate's output the
//! next prompt a repair reads.
//!
//! Two shell hazards disappear rather than being translated, and both are recorded in
//! the scripts because both cost a run:
//!
//!   * the fixture's two ids were read with `sed -n 1p` / `2p` because `mapfile` is
//!     bash 4 and macOS ships 3.2 — a gate that used it failed every branch with
//!     `mapfile: command not found`, three lines before anything was judged.
//!   * the CSV was passed to `python3 -` as an ARGUMENT, because `python3 -` reads its
//!     PROGRAM from stdin, so a heredoc and a pipe are the same channel: the heredoc
//!     wins, `csv.reader(sys.stdin)` reads nothing, and a correct candidate fails with
//!     "no rows at all".

mod gatelib;
use gatelib::{field, requires_capability, Gate};
use serde_json::{json, Value};

const CRATE: &str = "triage-domain";

fn start() -> Option<Gate> {
    Gate::compose_and_start("triage", CRATE, &[])
}

/// The two report ids the fixture seeds.
fn seeded(gate: &Gate) -> (String, String) {
    let seed = gate.seed();
    let ids: Vec<String> = seed["report_ids"]
        .as_array()
        .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
        .unwrap_or_default();
    assert!(
        ids.len() >= 2,
        "the fixture did not seed two reports — POST /test/seed answered: {seed}"
    );
    (ids[0].clone(), ids[1].clone())
}

fn parse(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or(Value::Null)
}

/// The CSV reader from `gate_clinic`, for the same reason: a test that needs a parser
/// to state its claim is testing the parser.
fn rows(text: &str) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let (mut row, mut cur, mut quoted) = (Vec::new(), String::new(), false);
        let mut chars = line.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '"' if quoted && chars.peek() == Some(&'"') => {
                    cur.push('"');
                    chars.next();
                }
                '"' => quoted = !quoted,
                ',' if !quoted => row.push(std::mem::take(&mut cur)),
                other => cur.push(other),
            }
        }
        row.push(cur);
        out.push(row);
    }
    out
}

#[test]
fn intake_masks_validates_and_deduplicates() {
    let Some(gate) = start() else { return };

    // The body carries an email, which is the point: the contract says what is STORED
    // is masked, so the raw address must not come back out.
    let body = json!({"title":"Search returns nothing","body":"contact me at ada@example.test","component":"search"});
    let (_, resp) = gate.post("/api/reports", None, body.clone());
    let id = field(&resp, "id");
    assert!(!id.is_empty(), "POST /api/reports returned no id: {resp}");

    let (_, stored) = gate.get(&format!("/api/reports/{id}"), None);
    assert!(
        !stored.contains("ada@example.test"),
        "the reporter's email was stored verbatim — it must be masked: {stored}"
    );
    assert!(
        stored.contains("[EMAIL]"),
        "the body was not masked with pii:redact's placeholder: {stored}"
    );

    let d = parse(&stored);
    assert_eq!(d["state"], "open", "a new report must be open with no severity: {stored}");
    assert!(
        d.get("severity").is_none() || d["severity"].is_null() || d["severity"] == "",
        "a new report must be open with no severity: {stored}"
    );

    // --- what a bad request is ----------------------------------------------------
    for (b, why) in [
        (json!({"title":"","body":"b","component":"c"}), "an empty title is a 400"),
        (json!({"title":"t","component":"c"}), "a missing body is a 400"),
        (json!({"title":"t","body":"b"}), "a missing component is a 400"),
    ] {
        let (c, _) = gate.post("/api/reports", None, b);
        assert_eq!(c, 400, "{why}");
    }
    let (c, _) = gate.send(
        "POST",
        "/api/reports",
        None,
        Some(("application/json", b"not json at all".to_vec())),
    );
    assert_eq!(c, 400, "malformed JSON is a 400");
    let (c, _) = gate.get("/api/reports/nope", None);
    assert_eq!(c, 404, "an unknown report is a 404");

    // --- the duplicate rule -------------------------------------------------------
    let (c, dup) = gate.post("/api/reports", None, body);
    assert_eq!(c, 409, "the same title in the same component is a duplicate");
    let existing = field(&dup, "existing");
    assert_eq!(
        existing, id,
        "a duplicate must name the report it collides with (got '{existing}', wanted '{id}')"
    );
    let (c, _) = gate.post(
        "/api/reports",
        None,
        json!({"title":"Search returns nothing","body":"b","component":"billing"}),
    );
    assert_eq!(c, 201, "the same title in a DIFFERENT component is not a duplicate");

    // --- listing and filtering ----------------------------------------------------
    gate.seed();
    let all = parse(&gate.get("/api/reports", None).1);
    assert!(
        all["reports"].as_array().map(|a| a.len() >= 4).unwrap_or(false),
        "GET /api/reports must list every report: {all}"
    );
    let filtered = parse(&gate.get("/api/reports?component=search", None).1);
    let rs = filtered["reports"].as_array().cloned().unwrap_or_default();
    assert!(
        !rs.is_empty() && rs.iter().all(|r| r["component"] == "search"),
        "?component= must filter: {filtered}"
    );
    let both = parse(&gate.get("/api/reports?state=open&component=billing", None).1);
    let rs = both["reports"].as_array().cloned().unwrap_or_default();
    assert!(
        !rs.is_empty() && rs.iter().all(|r| r["state"] == "open" && r["component"] == "billing"),
        "?state= and ?component= must AND: {both}"
    );
}

#[test]
fn workflow_is_a_machine_not_a_ladder_of_comparisons() {
    let Some(gate) = start() else { return };
    let (a, b) = seeded(&gate);

    for (body, code, why) in [
        (json!({"event":"explode"}), 400, "an unknown event is a 400"),
        (json!({}), 400, "a missing event is a 400"),
    ] {
        let (c, _) = gate.post(&format!("/api/reports/{a}/transition"), None, body);
        assert_eq!(c, code, "{why}");
    }
    let (c, _) = gate.post("/api/reports/nope/transition", None, json!({"event":"close"}));
    assert_eq!(c, 404, "an unknown report is a 404");

    // --- triage requires a severity ------------------------------------------------
    let (c, _) =
        gate.post(&format!("/api/reports/{a}/transition"), None, json!({"event":"triage"}));
    assert_eq!(c, 400, "the triage event requires a severity");
    let (c, _) = gate.post(
        &format!("/api/reports/{a}/transition"),
        None,
        json!({"event":"triage","severity":"urgent"}),
    );
    assert_eq!(c, 400, "a severity outside low/medium/high is a 400");

    // --- the legal path ------------------------------------------------------------
    let (_, resp) = gate.post(
        &format!("/api/reports/{a}/transition"),
        None,
        json!({"event":"triage","severity":"high"}),
    );
    let d = parse(&resp);
    assert_eq!(
        d["state"], "triaged",
        "triage must answer with the new state and the severity: {resp}"
    );
    assert_eq!(
        d["severity"], "high",
        "triage must answer with the new state and the severity: {resp}"
    );

    // The DOCUMENT must have moved too, not just the fsm instance. Read through the
    // SCAFFOLD's `/test/report/{id}`, not `GET /api/reports/{id}` — that route belongs
    // to `intake`, which is a stub while this part is judged alone. This gate once
    // asked intake for the document, got `{"error":"not_implemented"}`, and reported it
    // as `workflow` having failed to move it.
    let doc = parse(&gate.stored("report", &a));
    assert_eq!(doc["state"], "triaged", "the report document did not follow the fsm: {doc}");
    assert_eq!(doc["severity"], "high", "the report document did not follow the fsm: {doc}");

    // open -> fixed is not a legal jump
    let (c, _) = gate.post(&format!("/api/reports/{b}/transition"), None, json!({"event":"fix"}));
    assert_eq!(c, 409, "open cannot jump straight to fixed");

    // --- terminal really is terminal ------------------------------------------------
    let (c, _) = gate.post(&format!("/api/reports/{b}/transition"), None, json!({"event":"close"}));
    assert_eq!(c, 200, "open can be closed (not a bug)");
    let (c, _) = gate.post(
        &format!("/api/reports/{b}/transition"),
        None,
        json!({"event":"triage","severity":"low"}),
    );
    assert_eq!(c, 409, "a closed report is terminal and accepts nothing");

    // --- the queue -----------------------------------------------------------------
    //
    // Ordering is the whole check: severity first, no-severity last, older first
    // inside a severity. And a closed report is not in the queue at all.
    gate.post(&format!("/api/reports/{a}/transition"), None, json!({"event":"fix"}));
    let q = parse(&gate.get("/api/queue", None).1);
    let queue = q["queue"].as_array().cloned().unwrap_or_default();
    let ids: Vec<String> =
        queue.iter().filter_map(|r| r["id"].as_str().map(str::to_string)).collect();
    assert!(!ids.contains(&b), "a closed report must not be in the queue: {ids:?}");
    assert!(ids.contains(&a), "a fixed report is not closed, so it is still in the queue: {ids:?}");

    let rank = |s: Option<&str>| match s {
        Some("high") => 0,
        Some("medium") => 1,
        Some("low") => 2,
        _ => 3,
    };
    let keys: Vec<i32> = queue.iter().map(|r| rank(r["severity"].as_str())).collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(
        keys,
        sorted,
        "most urgent first, no severity last: {:?}",
        queue.iter().map(|r| r["severity"].clone()).collect::<Vec<_>>()
    );
    for r in &queue {
        for k in ["id", "title", "component", "state"] {
            assert!(r.get(k).is_some(), "a queue entry is missing {k}: {r}");
        }
    }
}

#[test]
fn digest_counts_only_what_occurs() {
    let Some(gate) = start() else { return };
    seeded(&gate);

    for path in ["/api/digest", "/api/digest?day=not-a-date", "/api/digest.csv"] {
        let (c, _) = gate.get(path, None);
        assert_eq!(c, 400, "a missing or unparseable day is a 400: {path}");
    }

    // The fixture writes both reports at 2026-08-17T09:00:00Z.
    let d = parse(&gate.get("/api/digest?day=2026-08-17", None).1);
    assert_eq!(d["day"], "2026-08-17", "the JSON digest is wrong: {d}");
    assert!(d["total"].as_u64().unwrap_or(0) >= 2, "the JSON digest is wrong: {d}");
    let (bs, bc) = (&d["by_state"], &d["by_component"]);
    assert!(bs.is_object() && bc.is_object(), "the JSON digest is wrong: {d}");
    assert!(bs["open"].as_u64().unwrap_or(0) >= 2, "both seeded reports are open: {bs}");
    // Only states/components that OCCUR are present — no zero-filled keys.
    for m in [bs, bc] {
        assert!(
            m.as_object().unwrap().values().all(|v| v.as_u64().unwrap_or(0) > 0),
            "no zero-filled keys: {m}"
        );
    }
    for want in ["auth", "billing"] {
        assert!(bc.get(want).is_some(), "by_component is missing {want}: {bc}");
    }
    assert!(d.get("open_high").is_some(), "the JSON digest is wrong: {d}");

    // A day with nothing in it is an empty digest, not a 404.
    let e = parse(&gate.get("/api/digest?day=1999-01-01", None).1);
    assert_eq!(e["total"], 0, "an empty day must still be a digest: {e}");
    assert_eq!(e["by_state"], json!({}), "an empty day must still be a digest: {e}");
    assert_eq!(e["by_component"], json!({}), "an empty day must still be a digest: {e}");

    // --- the CSV -------------------------------------------------------------------
    //
    // `Login fails, silently` is seeded precisely so that joining with commas produces
    // a row with six fields.
    let (_, csv) = gate.get("/api/digest.csv?day=2026-08-17", None);
    let r = rows(&csv);
    assert!(!r.is_empty(), "the CSV is wrong: no rows at all");
    assert_eq!(r[0], ["id", "title", "component", "state", "severity"], "header: {:?}", r[0]);
    let body = &r[1..];
    assert!(body.len() >= 2, "one row per report: {r:?}");
    for row in body {
        assert_eq!(
            row.len(),
            5,
            "every row has five columns — a comma in a title must be quoted: {row:?}"
        );
    }
    let titles: Vec<&String> = body.iter().map(|row| &row[1]).collect();
    assert!(
        titles.iter().any(|t| *t == "Login fails, silently"),
        "the comma-bearing title must survive intact: {titles:?}"
    );
    // severity is absent on a seeded report, and absent means EMPTY, not "null".
    let sev: Vec<&String> = body.iter().map(|row| &row[4]).collect();
    assert!(sev.iter().all(|s| *s != "null"), "an absent severity is an empty field: {sev:?}");

    // The content type has to be text/csv, or a browser and a parser both see JSON.
    let (_, ct, _) = gate.bytes("/api/digest.csv?day=2026-08-17", None);
    assert!(
        ct.starts_with("text/csv"),
        "the CSV must be served as text/csv, not '{ct}' — use Reply::raw"
    );

    // An empty day is the header alone.
    let (_, empty) = gate.get("/api/digest.csv?day=1999-01-01", None);
    let r = rows(&empty);
    assert!(r.len() == 1 && r[0][0] == "id", "an empty day is the header alone: {r:?}");
}

// ---------------------------------------------------------------------------
// the composition — the gate no single part can pass
// ---------------------------------------------------------------------------

/// One report driven the whole way through all three parts.
///
/// Ported from `components/triage-domain/e2e.sh`, the last of this app's four gates
/// to move. The three part gates came over in #180-#189; the nine COMPOSITION gates
/// did not, and nothing noticed because CI globbed `components/*/e2e-*.sh` — with a
/// hyphen — which matches the part gates and none of the `e2e.sh` files. #201 widened
/// the glob and found this one had been failing since the day after it was written.
///
/// The chain is why this goal has three parts and not two:
///
///   intake writes it -> workflow moves it and assigns severity -> digest counts it
///
/// `digest` can only be right if `intake` stored the contract's shape and `workflow`
/// updated the document rather than only the fsm instance. Each of those is a
/// plausible local success that shows up nowhere until the halves meet.
///
/// THE DAY COMES FROM THE COMPONENT. The shell version hardcoded `DAY=2026-08-17`,
/// its authoring date, while `intake` stamps `reported_at` from the store's own
/// `created` because the world imports no wall clock. It agreed with the app for one
/// day. The part gate never caught it: `e2e-digest.sh` reads the FIXTURE, whose
/// `reported_at` is a literal that never moves, so only the composition went through
/// the clock. Asking the component keeps that fixed by construction here.
#[test]
fn the_whole_triage_api_works() {
    let Some(gate) = start() else { return };

    // All three capabilities, in one place: a candidate that dropped one fails here
    // even if the part that owns it was never the one judged.
    requires_capability(CRATE, "pii:redact/redactor", "intake must mask the body with pii:redact");
    requires_capability(
        CRATE,
        "fsm:workflow/engine",
        "workflow must validate moves with the fsm engine",
    );
    requires_capability(CRATE, "csv:codec/codec", "digest must format the CSV with csv:codec");

    let (_, created) = gate.post(
        "/api/reports",
        None,
        json!({
            "title": "Totals drift, badly",
            "body": "ping me on +1 555 010 0199",
            "component": "billing",
        }),
    );
    let id = field(&created, "id");
    assert!(!id.is_empty(), "intake did not create a report: {created}");

    // The day the COMPONENT stamped, not a constant.
    let (_, doc) = gate.get(&format!("/api/reports/{id}"), None);
    let reported = parse(&doc)["reported_at"].as_str().unwrap_or_default().to_string();
    let day: String = reported.chars().take(10).collect();
    assert_eq!(day.len(), 10, "intake did not stamp a usable reported_at: {doc}");

    // A phone number is PII too, so intake masks more than emails.
    //
    // ELEVEN digits with a `+`, not `555-0100`. The scanner wants 10-15 digits and
    // either a leading `+` or a NANP-looking span, so a 7-digit local number is not a
    // phone number by its definition — asserting on one would have this gate "prove"
    // that masking was broken when it was working exactly as specified.
    assert!(!doc.contains("0199"), "the reporter's phone number was stored verbatim: {doc}");
    assert!(
        doc.contains("[PHONE]"),
        "the phone number was not masked with pii:redact's placeholder: {doc}"
    );

    // workflow moves it and assigns a severity.
    let (status, triaged) = gate.post(
        &format!("/api/reports/{id}/transition"),
        None,
        json!({"event": "triage", "severity": "high"}),
    );
    assert_eq!(
        status, 200,
        "workflow could not triage a report intake had just created: {triaged}"
    );

    // The queue is workflow's view; it must contain the report intake wrote.
    let (_, queue) = gate.get("/api/queue", None);
    let q = parse(&queue);
    let entry = q["queue"]
        .as_array()
        .and_then(|a| a.iter().find(|r| r["id"].as_str() == Some(id.as_str())))
        .unwrap_or_else(|| panic!("workflow's queue is missing intake's report: {queue}"));
    assert_eq!(entry["severity"], "high", "the queue lost the severity workflow assigned: {queue}");

    // --- digest sees what the other two did --------------------------------
    //
    // The assertion that needs all three parts to agree. `open_high` counts reports
    // with severity high that are not closed — a number that exists only because
    // intake wrote the document, workflow put `high` on it, and digest read it back.
    let (_, body) = gate.get(&format!("/api/digest?day={day}"), None);
    let d = parse(&body);
    assert!(
        d["open_high"].as_i64().unwrap_or(0) >= 1,
        "a high-severity open report was triaged; the digest missed it: {body}"
    );
    assert!(
        d["by_component"]["billing"].as_i64().unwrap_or(0) >= 1,
        "the digest does not reflect the triaged report: {body}"
    );
    assert!(
        d["by_state"]["triaged"].as_i64().unwrap_or(0) >= 1,
        "workflow moved a report to triaged and the document did not follow: {body}"
    );

    // And the CSV carries the severity workflow assigned, in the row for that report.
    let (_, csv) = gate.get(&format!("/api/digest.csv?day={day}"), None);
    let parsed = rows(&csv);
    assert_eq!(
        parsed[0],
        vec!["id", "title", "component", "state", "severity"],
        "the CSV header is not the contract's: {csv}"
    );
    let row = parsed[1..]
        .iter()
        .find(|r| r.first().map(String::as_str) == Some(id.as_str()))
        .unwrap_or_else(|| panic!("the report is missing from the CSV: {csv}"));
    assert_eq!(row.len(), 5, "the row lost a column: {csv}");
    assert_eq!(row[3], "triaged", "the CSV disagrees about the state: {csv}");
    assert_eq!(row[4], "high", "the CSV does not carry what workflow assigned: {csv}");
    // The comma-bearing title still has to survive alongside it.
    let titles: Vec<&str> = parsed[1..].iter().map(|r| r[1].as_str()).collect();
    assert!(
        titles.contains(&"Totals drift, badly"),
        "a comma in a title must be quoted, not split: {titles:?}"
    );

    // --- closing takes it out of the queue, and the digest agrees ----------
    for event in ["fix", "close"] {
        let (status, out) =
            gate.post(&format!("/api/reports/{id}/transition"), None, json!({"event": event}));
        assert_eq!(status, 200, "`{event}` was refused: {out}");
    }

    let (_, queue) = gate.get("/api/queue", None);
    let after = parse(&queue);
    let ids: Vec<&str> = after["queue"]
        .as_array()
        .map(|a| a.iter().filter_map(|r| r["id"].as_str()).collect())
        .unwrap_or_default();
    assert!(
        !ids.contains(&id.as_str()),
        "a closed report is still in the queue — closed reports are not queued: {queue}"
    );

    let (_, body) = gate.get(&format!("/api/digest?day={day}"), None);
    assert!(
        parse(&body)["by_state"]["closed"].as_i64().unwrap_or(0) >= 1,
        "the digest still counts the closed report as open_high: {body}"
    );

    // A closed report no longer blocks a new one with the same title+component: the
    // bug came back. Intake's rule, checked here because it depends on workflow
    // having closed it — neither part can assert this alone.
    let (status, again) = gate.post(
        "/api/reports",
        None,
        json!({"title": "Totals drift, badly", "body": "again", "component": "billing"}),
    );
    assert_eq!(
        status, 201,
        "a closed report must not block a new report with the same title: {again}"
    );
}
