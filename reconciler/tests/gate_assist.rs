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
    assert!(!t.is_empty(), "POST /test/token returned no token — the scaffold is broken, not the part");
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
    assert!(!id.is_empty(), "the fixture produced no reports — the scaffold is broken, not the part");
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
        "confidence is classify's 0..=1000 milli-units, passed through as-is, got {:?}", a["confidence"]
    );
    let s = a["summary"].as_str().unwrap_or_default().trim().to_string();
    assert!((10..=600).contains(&s.len()), "no usable summary, got {} chars: {s:?}", s.len());

    // Not a copy: an extractive slice of the input needs no model at all.
    let title = before["title"].as_str().unwrap_or_default();
    let haystack = format!("{title}\n{}", before["body"].as_str().unwrap_or_default());
    assert!(!haystack.contains(&s), "the summary is a verbatim slice of the report — that is extraction, not a model call");
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
    assert!(!stored.trim().is_empty(), "the route answered an empty body — it is not implemented, or it trapped");
    let d = parse(&stored);
    let block = &d["assist"];
    assert!(block.is_object(), "the report has no assist block: {d}");
    assert_eq!(block["severity"], a["severity"], "the stored severity differs from the answer: {block} vs {a}");
    assert_eq!(block["summary"], a["summary"], "the stored summary differs from the answer");
    assert!(
        block["assisted_at"].as_str().unwrap_or_default().ends_with('Z'),
        "assisted_at must be RFC3339 UTC: {:?}", block["assisted_at"]
    );

    // Reading it back through the part's own route.
    let got = parse(&gate.get(&format!("/api/reports/{id}/assist"), Some(&t)).1);
    assert_eq!(got["severity"], a["severity"], "GET /api/reports/{{id}}/assist did not answer the stored assist: {got}");

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
