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
use gatelib::{field, Gate};
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
