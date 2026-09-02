//! The `triage:assist` gate, ported from
//! `components/triage-assist-domain/e2e-assist.sh`.
//!
//! The last of the thirty-one, and the only one that starts TWO hosts: the second has
//! its provider pointed at a closed port, because "what happens when the model is
//! unreachable" is a different deployment rather than a different request. Port 1
//! refuses immediately, so it costs a connection attempt rather than a timeout.
//!
//! Verified against `mlx-community/Qwen3.8-27B-4bit` on csatapaci through
//! `just openai-shim`.

mod gatelib;
use gatelib::{field, Gate, Shim};
use serde_json::{json, Value};

const CRATE: &str = "triage-assist-domain";

fn parse(t: &str) -> Value {
    serde_json::from_str(t.trim()).unwrap_or(Value::Null)
}
fn token(gate: &Gate) -> String {
    let t = field(&gate.post("/test/token", None, json!({"subject":"ada"})).1, "token");
    assert!(
        !t.is_empty(),
        "POST /test/token returned no token — the scaffold is broken, not the part"
    );
    t
}
/// The first seeded report id.
fn seed_one(gate: &Gate) -> String {
    let id = gate.seed()["report_ids"]
        .as_array()
        .and_then(|a| a.first())
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    assert!(
        !id.is_empty(),
        "the fixture produced no reports — the scaffold is broken, not the part"
    );
    id
}

#[test]
fn an_assist_is_written_once_and_never_written_at_all_when_the_model_is_down() {
    let Some(shim) = Shim::probe("triage-assist/assist") else { return };

    // --- with the model reachable --------------------------------------------------
    let config = shim.config();
    let cfg: Vec<&str> = config.iter().map(String::as_str).collect();
    let egress = shim.egress();
    let Some(gate) = Gate::compose_and_start_with_egress("triage-assist", CRATE, &cfg, &[&egress])
    else {
        return;
    };

    let t = token(&gate);
    let id = seed_one(&gate);
    let before = parse(&gate.stored("report", &id));

    let (_, raw) = gate.json("POST", &format!("/api/reports/{id}/assist"), Some(&t), None);
    let a = parse(&raw);
    let sev = a["severity"].as_str().unwrap_or_default();
    assert!(
        ["critical", "major", "minor"].contains(&sev),
        "severity must be one of the three labels the model was given, got {sev:?} (whole answer: {a})"
    );
    let conf = a["confidence"].as_i64();
    assert!(
        conf.is_some_and(|c| (0..=1000).contains(&c)),
        "confidence is classify's 0..=1000 milli-units, passed through as-is, got {:?}",
        a["confidence"]
    );
    let s = a["summary"].as_str().unwrap_or_default().trim().to_string();
    assert!((10..=600).contains(&s.len()), "no usable summary, got {} chars: {s:?}", s.len());

    // Not a copy: an extractive slice of the input needs no model at all.
    let title = before["title"].as_str().unwrap_or_default();
    let haystack = format!("{title}\n{}", before["body"].as_str().unwrap_or_default());
    assert!(
        !haystack.contains(&s),
        "the summary is a verbatim slice of the report — that is extraction, not a model call"
    );
    assert_ne!(s, title, "the summary is the title again");
    // About THIS report: a canned sentence passes every check above.
    let low = s.to_lowercase();
    assert!(
        ["safari", "button", "white", "checkout", "banner", "invisible", "render"]
            .iter()
            .any(|w| low.contains(w)),
        "the summary mentions nothing from the report — it is not about this report: {s:?}"
    );

    // The same fields on the document, which is where the next reader looks.
    let stored = gate.stored("report", &id);
    assert!(
        !stored.trim().is_empty(),
        "the route answered an empty body — it is not implemented, or it trapped"
    );
    let d = parse(&stored);
    let block = &d["assist"];
    assert!(block.is_object(), "the report has no assist block: {d}");
    assert_eq!(
        block["severity"], a["severity"],
        "the stored severity differs from the answer: {block} vs {a}"
    );
    assert_eq!(block["summary"], a["summary"], "the stored summary differs from the answer");
    assert!(
        block["assisted_at"].as_str().unwrap_or_default().ends_with('Z'),
        "assisted_at must be RFC3339 UTC: {:?}",
        block["assisted_at"]
    );

    // Reading it back through the part's own route.
    let got = parse(&gate.get(&format!("/api/reports/{id}/assist"), Some(&t)).1);
    assert_eq!(
        got["severity"], a["severity"],
        "GET /api/reports/{{id}}/assist did not answer the stored assist: {got}"
    );

    // A second assist is a conflict, not a second model call — otherwise a refresh is a
    // model call per request forever.
    let (_, again) = gate.json("POST", &format!("/api/reports/{id}/assist"), Some(&t), None);
    let (code, _) = gate.json("POST", &format!("/api/reports/{id}/assist"), Some(&t), None);
    assert_eq!(code, 409, "assisting an already-assisted report must be 409");
    let d = parse(&again);
    assert_eq!(d["error"], "already_assisted", "{d}");
    assert!(
        ["critical", "major", "minor"].contains(&d["severity"].as_str().unwrap_or_default()),
        "the 409 must carry the stored severity: {d}"
    );

    // The refusals that cost nothing.
    let (c, _) = gate.json("POST", "/api/reports/nope/assist", Some(&t), None);
    assert_eq!(c, 404, "assisting an unknown report must be 404");
    let (c, _) = gate.json("POST", &format!("/api/reports/{id}/assist"), None, None);
    assert_eq!(c, 401, "assisting with no bearer must be 401");

    // --- with the model unreachable ------------------------------------------------
    //
    // A second host, provider pointed at a closed port. Dropping the first is what makes
    // this a different DEPLOYMENT rather than a different request.
    drop(gate);
    let Some(down) = Gate::compose_and_start_with_egress(
        "triage-assist",
        CRATE,
        &["anthropic:base-url=http://127.0.0.1:1", "anthropic:timeout=5"],
        &["127.0.0.1:1"],
    ) else {
        return;
    };
    let t = token(&down);
    let down_id = seed_one(&down);

    let (c, _) = down.json("POST", &format!("/api/reports/{down_id}/assist"), Some(&t), None);
    assert_eq!(c, 503, "a provider that cannot be reached must be 503 assist_unavailable");

    let d = parse(&down.stored("report", &down_id));
    assert!(
        d.get("assist").is_none(),
        "the provider was down and the report was written anyway — a report with an empty opinion attached: {d}"
    );
}

// ---------------------------------------------------------------------------
// the composition — the gate no single part can pass
// ---------------------------------------------------------------------------

/// The whole triage-assist API: a report taken in, assisted by the model, and the
/// audit trail proving what the other two parts did.
///
/// Ported from `components/triage-assist-domain/e2e.sh`. One model call, like the
/// assist gate's own — the cost of this gate is what the loop pays per attempt, and
/// two would double it for nothing.
///
/// The trail IS the join. `intake` writes `reports.create` and `assist` writes
/// `reports.assist`, and the ledger part reads both. A part that invented its own
/// storage shape or its own event names passes its own gate and fails here.
///
/// The subject check is the sharpest of them: every audit event must carry
/// `principal.subject` — what `authorize` RETURNED — and not the bearer token. A token
/// in an audit trail is both the wrong value and a credential written to a log, and
/// the failure names which part wrote it, because a verdict addressed to nobody
/// cannot be repaired.
#[test]
fn the_whole_triage_assist_api_works() {
    let Some(shim) = Shim::probe("triage-assist/whole") else { return };
    let mut config = shim.config();
    // Three accepted reports before the lockout, and the third is this gate's own —
    // so the limit must be at least 4 for the happy path to survive it.
    config.push("max-attempts=4".into());
    config.push("lockout-window=60".into());
    let cfg: Vec<&str> = config.iter().map(String::as_str).collect();
    let egress = shim.egress();
    let Some(gate) = Gate::compose_and_start_with_egress("triage-assist", CRATE, &cfg, &[&egress])
    else {
        return;
    };

    for (iface, why) in [
        (
            "ratelimit:guard/limiter",
            "the composed API must still be counting attempts through the limiter component",
        ),
        ("pii:redact/redactor", "the composed API must still be masking through pii-redact"),
        (
            "ai:inference/inference",
            "the composed API must still be reaching the model through ai-inference",
        ),
        ("audit:log/recorder", "the composed API must still be recording through audit-log"),
    ] {
        gatelib::requires_capability(CRATE, iface, why);
    }

    const TRACE: &str = "deadbeefdeadbeefdeadbeefdeadbee1";
    let tp = format!("00-{TRACE}-0000000000000001-01");
    let t = field(&gate.post("/test/token", None, json!({"subject":"ada"})).1, "token");
    assert!(
        !t.is_empty(),
        "POST /test/token returned no token — the scaffold is broken, not the parts"
    );

    let traced = |method: &str, path: &str, body: Option<Value>| {
        gate.with_headers(method, path, Some(&t), &[("traceparent", tp.as_str())], body)
    };

    // --- a report goes all the way through ---------------------------------
    let (_, resp) = traced(
        "POST",
        "/api/reports",
        Some(json!({
            "title": "Checkout total is wrong",
            "body": "off by a cent, mail me at ada@example.test",
            "component": "billing",
        })),
    );
    let id = field(&resp, "id");
    assert!(!id.is_empty(), "POST /api/reports returned no id: {resp}");

    let (_, read) = traced("GET", &format!("/api/reports/{id}"), None);
    assert!(
        !read.contains("ada@example.test"),
        "the composed API stored the reporter's email verbatim: {read}"
    );

    let (_, assist) = traced("POST", &format!("/api/reports/{id}/assist"), None);
    let a = parse(&assist);
    let severity = a["severity"].as_str().unwrap_or_default();
    assert!(["critical", "major", "minor"].contains(&severity), "no usable severity: {assist}");
    assert!(
        a["summary"].as_str().unwrap_or_default().trim().len() >= 20,
        "no usable summary: {assist}"
    );

    // --- the trail proves what the other two did ---------------------------
    let events = |gate: &Gate| -> Vec<Value> {
        let (_, e) = gate.get(&format!("/api/audit?trace={TRACE}"), Some(&t));
        parse(&e)["events"].as_array().cloned().unwrap_or_default()
    };
    let evs = events(&gate);
    let pairs: Vec<(String, String)> = evs
        .iter()
        .map(|e| {
            (
                e["event"].as_str().unwrap_or_default().to_string(),
                e["outcome"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    let has = |name: &str, outcome: &str| pairs.iter().any(|(n, o)| n == name && o == outcome);
    assert!(
        has("reports.create", "ok"),
        "no reports.create/ok in the trail: the intake part did not note an accepted report \
         under the contract's name. Got {pairs:?}"
    );
    assert!(
        has("reports.assist", "ok"),
        "no reports.assist/ok in the trail: the assist part did not note the model's answer. \
         Got {pairs:?}"
    );

    // Named per part, because a verdict addressed to nobody cannot be repaired.
    fn owner(name: &str) -> &str {
        match name {
            "reports.create" => "intake (src/intake.rs)",
            "reports.assist" => "assist (src/assist.rs)",
            other => other,
        }
    }
    let mut wrong: Vec<String> = evs
        .iter()
        .filter(|e| e["event"].as_str() != Some("http.request"))
        .filter(|e| e["subject"].as_str() != Some("ada"))
        .map(|e| {
            format!(
                "    {} wrote subject={:?}",
                owner(e["event"].as_str().unwrap_or_default()),
                e["subject"]
            )
        })
        .collect();
    wrong.sort();
    wrong.dedup();
    assert!(
        wrong.is_empty(),
        "an audit event carries something other than the principal's subject ('ada'):\n{}\n  \
         The subject is principal.subject — what `authorize` RETURNED. A bearer token there \
         is both the wrong value and a credential written into an audit trail.",
        wrong.join("\n")
    );

    // --- and the limit is still a limit once everything is wired together --
    for i in 1..=3 {
        traced(
            "POST",
            "/api/reports",
            Some(json!({"title": format!("noise {i}"), "body": "b", "component": "web"})),
        );
    }
    let (got, _) = traced(
        "POST",
        "/api/reports",
        Some(json!({"title":"one too many","body":"b","component":"web"})),
    );
    assert_eq!(got, 429, "past the limit the composed API must answer 429, got {got}");

    let after = events(&gate);
    assert!(
        after.iter().any(|e| {
            e["event"].as_str() == Some("reports.create")
                && e["outcome"].as_str() == Some("throttled")
        }),
        "a throttled report left no trace — an operator cannot tell a rate limit from an \
         outage. The intake part refused for the rate limit and noted nothing under the \
         contract's outcome name."
    );
}
