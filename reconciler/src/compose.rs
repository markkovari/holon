//! Joining the parts of a decomposed goal: the mocks that let them diverge, the
//! merge that puts them back together, and the gate that judges the whole.
//!
//! Two green parts are not a green whole (ADR-0086). Each part gates alone —
//! the backend against the contract, the frontend against fixtures generated from
//! the same contract — and neither waits for the other. This module is what
//! happens after both are green: merge the winners, run the goal's own checks over
//! the joined tree, and refuse loudly when the halves were built against different
//! versions.
//!
//! The gate here is **the same runner** the branches were judged by. A composition
//! judged by different machinery than the parts is a composition whose failures
//! are arguments about the harness.

use std::time::Duration;

use serde_json::{json, Value};

use crate::generation::Entry;

/// Where fixtures generated from the contract are laid in a part's tree.
pub const MOCK_DIR: &str = ".contract-mocks";

/// How a part asks for a change to the contract.
///
/// A model has no tool: it cannot call the registry, and giving it one would mean
/// an agent that can reach the shared interface directly. So a request rides the
/// channel a branch already has — **a file in its candidate**. First line is the
/// subject, the rest is the body, and the orchestrator turns it into an `ask` and
/// then drops it: it is a question, not code, and merging it would land a
/// question in the pull request.
pub const REQUEST_PATH: &str = "CONTRACT-REQUEST.md";

/// The request a candidate is carrying, if any.
pub fn request_of(entry: &Entry) -> Option<(String, String)> {
    let files = entry.files.as_array()?;
    let f = files.iter().find(|f| f["path"] == json!(REQUEST_PATH))?;
    let content = f["content"].as_str()?.trim();
    let (subject, body) = match content.split_once('\n') {
        Some((s, b)) => (s.trim(), b.trim()),
        // A subject with no body is a question with no argument. Allowed — the
        // answering part still has the contract and the subject — but the body
        // being empty is not an error worth failing a round over.
        None => (content, ""),
    };
    if subject.is_empty() {
        return None;
    }
    Some((subject.to_string(), body.to_string()))
}

/// Every distinct request in a generation's worth of candidates.
///
/// Read from ALL the entries, not from the winner: a branch that scored zero can
/// still be the one that noticed the interface is wrong, and when no branch passes
/// there is no winner to read at all — which is exactly the round in which a part
/// most needs to ask for something.
pub fn requests_in(entries: &[Entry]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for e in entries {
        if let Some((subject, body)) = request_of(e) {
            if !out.iter().any(|(s, _)| *s == subject) {
                out.push((subject, body));
            }
        }
    }
    out
}

/// Fixtures a part can develop against, derived from the contract.
///
/// This is "mock generation" in the only form that costs nothing and works in
/// every language: **files**, not a server. A generated mock server needs a
/// runtime per stack, a port, and something to keep it alive for the length of a
/// check; a fixture is a file any test framework can read, and the part's tree is
/// already how a branch is handed everything else it knows.
///
/// The contract's own `example` is the fixture. A route without one produces no
/// file, which is deliberate: inventing an example would put a shape into the
/// frontend's tests that the backend never agreed to.
pub fn mocks(contract: &str) -> Vec<(String, String)> {
    let Ok(v) = serde_json::from_str::<Value>(contract) else {
        // A contract that is not JSON is a contract this cannot read, and a
        // fixture guessed at from prose is worse than no fixture.
        return Vec::new();
    };
    let mut out = Vec::new();
    for route in v["routes"].as_array().cloned().unwrap_or_default() {
        let (Some(method), Some(path)) = (route["method"].as_str(), route["path"].as_str()) else {
            continue;
        };
        let example = &route["example"];
        if example.is_null() {
            continue;
        }
        out.push((
            format!("{MOCK_DIR}/{}", fixture_name(method, path)),
            serde_json::to_string_pretty(example).unwrap_or_else(|_| example.to_string()),
        ));
    }
    out.sort();
    out
}

/// `GET /api/search?q=` → `GET_api_search.json`. A path is not a filename.
fn fixture_name(method: &str, path: &str) -> String {
    let cleaned: String = path
        .split('?')
        .next()
        .unwrap_or(path)
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    format!("{}_{}.json", method.to_uppercase(), cleaned.trim_matches('_'))
}

/// Lay the generated fixtures into a part's plan, beside the contract.
pub fn with_mocks(plan: &Value, contract: &str) -> Value {
    let mut plan = plan.clone();
    let mut context = plan["context"].as_array().cloned().unwrap_or_default();
    for (path, content) in mocks(contract) {
        let entry = json!({ "path": path, "content": content });
        match context.iter_mut().find(|c| c["path"] == json!(path)) {
            Some(existing) => *existing = entry,
            None => context.push(entry),
        }
    }
    plan["context"] = Value::Array(context);
    plan
}

/// Every part's winner, as one set of changes.
///
/// Two parts writing the same path is refused rather than resolved. Their
/// `writable` sets are supposed to be disjoint — that is most of what makes them
/// separate parts — so an overlap is a decomposition bug, and silently taking one
/// side of it would land half of each.
pub fn merge(winners: &[(String, Entry)]) -> Result<Value, Vec<String>> {
    let mut merged: Vec<Value> = Vec::new();
    let mut owner: Vec<(String, String)> = Vec::new();
    let mut conflicts = Vec::new();
    for (part, entry) in winners {
        for f in entry.files.as_array().cloned().unwrap_or_default() {
            let path = f["path"].as_str().unwrap_or_default().to_string();
            // A request is a question, not code. It has already been asked by the
            // time anything is merged, and landing it would put a question in the
            // pull request.
            if path == REQUEST_PATH {
                continue;
            }
            match owner.iter().find(|(p, _)| *p == path) {
                Some((_, other)) => conflicts.push(format!(
                    "{path} was written by both {other} and {part} — parts must not share a \
                     writable path"
                )),
                None => {
                    owner.push((path, part.clone()));
                    merged.push(f);
                }
            }
        }
    }
    if !conflicts.is_empty() {
        return Err(conflicts);
    }
    Ok(Value::Array(merged))
}

/// What the composition gate found.
#[derive(Debug, Clone)]
pub struct Report {
    pub passed: bool,
    pub score: u64,
    /// One line per failing check — what a human acts on.
    pub failures: Vec<String>,
}

/// Run the goal's own checks over the joined tree, on the same runner that judged
/// the parts.
pub fn gate(
    checks_url: &str,
    base_commit: &str,
    base_tree: &Value,
    changes: &Value,
    checks: &Value,
    timeout: Duration,
) -> Result<Report, String> {
    if checks.as_array().map(|c| c.is_empty()).unwrap_or(true) {
        // The same refusal the evaluator makes: an empty gate accepts everything,
        // which is never what was meant.
        return Err("no composition checks — an empty gate would accept any two halves".into());
    }
    let body = json!({
        "candidate": "composition",
        "base_commit": base_commit,
        "base_tree": base_tree,
        "changes": changes,
        "checks": checks,
    });
    let r = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| e.to_string())?
        .post(checks_url)
        .body(body.to_string())
        .send()
        .map_err(|e| format!("{e}"))?;
    let text = r.text().unwrap_or_default();
    let v: Value =
        serde_json::from_str(&text).map_err(|e| format!("unreadable report ({e}): {text}"))?;
    Ok(report_of(&v))
}

/// The runner's report, read the way the evaluator reads it.
///
/// Two field names to get right, both captured from `comp-checks` rather than
/// guessed: the per-check list is **`results`**, and the gate is **`accepted`** —
/// `passed` at the top level is a COUNT of how many checks passed, so reading it
/// as a boolean makes every composition fail with an empty list of reasons, which
/// is precisely as confusing as it sounds.
pub fn report_of(v: &Value) -> Report {
    let outcomes = v["results"].as_array().cloned().unwrap_or_default();
    let failures: Vec<String> = outcomes
        .iter()
        .filter(|o| o["passed"] != json!(true))
        .map(|o| {
            format!(
                "{}: {}",
                o["id"].as_str().unwrap_or("?"),
                o["detail"].as_str().unwrap_or("failed").lines().take(3).collect::<Vec<_>>().join(" ")
            )
        })
        .collect();
    Report {
        // The runner has already decided the gate; trusting its verdict rather than
        // re-deriving it keeps the gate in one place. `failures` is belt and braces
        // for a report that says accepted with a required check missing.
        passed: v["accepted"].as_bool().unwrap_or(false) && failures.is_empty(),
        score: v["score"].as_u64().unwrap_or(0),
        failures,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(files: Value) -> Entry {
        Entry {
            branch: "branch-0".into(),
            accepted: true,
            score: 1000,
            digest: "d".into(),
            spent_tokens: 0,
            attempts: 1,
            files,
            failures: json!([]),
            note: String::new(),
            elapsed_ms: 0,
            stopped: "accepted".into(),
        }
    }

    #[test]
    fn a_route_with_an_example_becomes_a_fixture_and_one_without_does_not() {
        let contract = r#"{"routes":[
            {"method":"get","path":"/api/search?q=","example":{"hits":[],"has_more":false}},
            {"method":"POST","path":"/api/index"}
        ]}"#;
        let out = mocks(contract);
        assert_eq!(out.len(), 1, "a route with no example invents nothing: {out:?}");
        assert_eq!(out[0].0, ".contract-mocks/GET_api_search.json");
        assert!(out[0].1.contains("has_more"));
        // Pretty-printed, because a human reads these in a diff.
        assert!(out[0].1.contains('\n'), "{}", out[0].1);
    }

    #[test]
    fn a_contract_that_is_not_json_generates_nothing_rather_than_guessing() {
        assert!(mocks("GET /api/search -> { hits }").is_empty());
        assert!(mocks("").is_empty());
    }

    #[test]
    fn fixtures_are_laid_beside_the_contract_and_replaced_not_stacked() {
        let plan = json!({ "text": "build the ui", "context": [] });
        let v1 = with_mocks(&plan, r#"{"routes":[{"method":"GET","path":"/a","example":{"v":1}}]}"#);
        assert_eq!(v1["context"].as_array().unwrap().len(), 1);
        let v2 = with_mocks(&v1, r#"{"routes":[{"method":"GET","path":"/a","example":{"v":2}}]}"#);
        let ctx = v2["context"].as_array().unwrap();
        assert_eq!(ctx.len(), 1, "an amended contract replaces its fixtures");
        assert!(ctx[0]["content"].as_str().unwrap().contains("\"v\": 2"));
    }

    #[test]
    fn two_parts_are_one_set_of_changes() {
        let winners = vec![
            ("backend".to_string(), entry(json!([{ "path": "src/api.rs", "content": "be" }]))),
            ("frontend".to_string(), entry(json!([{ "path": "ui/app.ts", "content": "fe" }]))),
        ];
        let merged = merge(&winners).expect("disjoint parts merge");
        assert_eq!(merged.as_array().unwrap().len(), 2);
    }

    #[test]
    fn two_parts_writing_one_path_is_refused_and_says_which() {
        let winners = vec![
            ("backend".to_string(), entry(json!([{ "path": "shared/dto.ts", "content": "be" }]))),
            ("frontend".to_string(), entry(json!([{ "path": "shared/dto.ts", "content": "fe" }]))),
        ];
        let conflicts = merge(&winners).expect_err("an overlap is a decomposition bug");
        assert_eq!(conflicts.len(), 1);
        assert!(conflicts[0].contains("shared/dto.ts"), "{}", conflicts[0]);
        assert!(conflicts[0].contains("backend"), "{}", conflicts[0]);
        assert!(conflicts[0].contains("frontend"), "{}", conflicts[0]);
    }

    #[test]
    fn a_candidate_may_carry_a_question_and_it_is_not_code() {
        let asking = entry(json!([
            { "path": "ui/app.ts", "content": "render()" },
            { "path": REQUEST_PATH, "content": "SearchResult needs total_pages\nI cannot paginate from next_cursor alone.\n" }
        ]));
        let (subject, body) = request_of(&asking).expect("the candidate asked for something");
        assert_eq!(subject, "SearchResult needs total_pages");
        assert!(body.starts_with("I cannot paginate"));

        // And the question does not land.
        let merged = merge(&[("frontend".to_string(), asking)]).unwrap();
        let paths: Vec<&str> =
            merged.as_array().unwrap().iter().map(|f| f["path"].as_str().unwrap()).collect();
        assert_eq!(paths, ["ui/app.ts"], "a question in the pull request is not an answer");
    }

    #[test]
    fn a_question_from_a_losing_branch_is_still_a_question() {
        let asking = entry(json!([
            { "path": REQUEST_PATH, "content": "SearchResult needs total_pages\nbecause pagers" }
        ]));
        let silent = entry(json!([{ "path": "ui/app.ts", "content": "x" }]));
        let out = requests_in(&[silent.clone(), asking.clone()]);
        assert_eq!(out.len(), 1, "the round nobody passed has no winner to read");
        assert_eq!(out[0].0, "SearchResult needs total_pages");
        assert_eq!(requests_in(&[asking.clone(), asking]).len(), 1, "four branches, one question");
        assert!(requests_in(&[silent]).is_empty());
    }

    #[test]
    fn a_candidate_with_no_question_asks_nothing() {
        assert!(request_of(&entry(json!([{ "path": "ui/app.ts", "content": "x" }]))).is_none());
        // An empty request file is not a request: a model that wrote the file and
        // nothing in it has not asked anybody anything.
        assert!(request_of(&entry(json!([{ "path": REQUEST_PATH, "content": "  \n " }]))).is_none());
    }

    #[test]
    fn an_empty_gate_is_refused_rather_than_passed() {
        let e = gate("http://127.0.0.1:1", "c", &json!([]), &json!([]), &json!([]), Duration::from_secs(1))
            .expect_err("an empty gate accepts everything");
        assert!(e.contains("empty gate"), "{e}");
    }

    /// The real shape, captured from `comp-checks`. Note `passed` is a COUNT.
    #[test]
    fn a_report_names_what_failed() {
        let v = json!({
            "candidate": "composition",
            "accepted": false,
            "score": 400,
            "passed": 1,
            "total": 2,
            "results": [
                { "id": "backend-tests", "passed": true, "detail": "" },
                { "id": "join", "passed": false, "detail": "expected has_more, found total_pages\nat ui/app.ts:12" }
            ]
        });
        let r = report_of(&v);
        assert!(!r.passed);
        assert_eq!(r.score, 400);
        assert_eq!(r.failures.len(), 1);
        assert!(r.failures[0].starts_with("join: expected has_more"), "{}", r.failures[0]);
    }

    #[test]
    fn a_green_report_is_green() {
        let v = json!({
            "candidate": "composition", "accepted": true, "score": 1000, "passed": 3, "total": 3,
            "results": [ { "id": "join", "passed": true } ]
        });
        let r = report_of(&v);
        assert!(r.passed);
        assert!(r.failures.is_empty());
    }
}

// ===========================================================================
// The whole decomposed run, in one place.
// ===========================================================================
//
// This lived in `comp-goalrun` and again in the e2e that was supposed to cover
// it, which meant the test exercised a re-spelling of the binary rather than the
// binary. Two copies of an orchestration drift, and the one that drifts is always
// the one nothing runs — the same reason the SurrealDB fixture moved into the
// shared harness. So the loop is library code with a thin caller: the binary
// prints and lands, the test asserts, and both drive THIS.

use crate::contract::{Answerer, Ask, Registry};
use crate::generation::{compose_search, Bounds, Composition, Part};

/// What a decomposed run came to, in the order a caller has to check it.
pub struct Composed {
    pub composition: Composition,
    /// Every negotiation line, in order — the part a reviewer most needs and
    /// could never reconstruct.
    pub log: Vec<String>,
    /// Why there is nothing to land, or empty. A part that never passed, halves on
    /// different contract versions, two parts writing one path, a join that failed:
    /// four different problems, each of which must name itself.
    pub blocked: Vec<String>,
    /// The merged tree, when there is one.
    pub changes: Option<Value>,
    /// The composition gate's verdict, when it ran.
    pub report: Option<Report>,
}

impl Composed {
    /// There is a joined tree that passed a gate neither half could pass alone.
    pub fn landable(&self) -> bool {
        self.blocked.is_empty()
            && self.changes.is_some()
            && self.report.as_ref().map(|r| r.passed).unwrap_or(false)
    }
}

/// Where the parts run, what judges them, and who they agree through.
pub struct Wiring<'a> {
    pub driver_url: &'a str,
    pub driver_host: &'a str,
    pub checks_url: &'a str,
    pub registry: &'a Registry,
    /// `None` runs the loop without answering anything: requests accumulate, the
    /// run continues on the current contract, and nothing blocks. A supported way
    /// to run and the shape a deployment with no provider gets.
    pub answerer: Option<&'a Answerer>,
}

/// Run a decomposed goal to the point where a pull request could be opened.
///
/// Deliberately does no printing and opens nothing: what a run should SAY and
/// whether it should land are the caller's, and a function that did both could not
/// be tested without a forge.
#[allow(clippy::too_many_arguments)]
pub fn run_parts(
    w: &Wiring,
    parts: &[Part],
    contract: &str,
    version: u32,
    bounds: Bounds,
    seed: u64,
    timeout: Duration,
    base_commit: &str,
    base_tree: &Value,
    composition_checks: &Value,
) -> Composed {
    let mut log: Vec<String> = Vec::new();

    let composition = compose_search(
        w.driver_url,
        w.driver_host,
        parts,
        contract,
        version,
        bounds,
        seed,
        timeout,
        |round, outcomes| {
            // A part asks by writing a file, and the orchestrator turns it into a
            // request: a model has no tool, and giving it one would mean an agent
            // that can reach the shared interface directly.
            for o in outcomes {
                let Some(last) = o.rounds.last() else { continue };
                for (subject, body) in requests_in(&last.entries) {
                    // Addressed to the other part. With more than two this needs a
                    // routing rule; with two it is the only other one there is.
                    let Some(to) = parts.iter().map(|p| &p.name).find(|n| **n != o.part) else {
                        continue;
                    };
                    let ask = Ask {
                        id: String::new(),
                        from_part: o.part.clone(),
                        to_part: to.clone(),
                        subject: subject.clone(),
                        body,
                        at_version: o.built_against.max(1),
                    };
                    match w.registry.ask(&ask) {
                        Ok(_) => log
                            .push(format!("generation {round}: {} asked {to} for {subject:?}", o.part)),
                        Err(e) => log.push(format!("generation {round}: could not ask: {e}")),
                    }
                }
            }
            match w.registry.boundary(outcomes, w.answerer, &mut log) {
                Ok(next) => next,
                // A boundary that cannot say what the contract is has nothing to
                // hand the next round, and carrying on from a guess is how two
                // halves that each pass fail together. The round repeats on what
                // each part had.
                Err(e) => {
                    log.push(format!("generation {round}: the boundary failed: {e}"));
                    Vec::new()
                }
            }
        },
    );

    // 1. Every part, or nothing. A brilliant backend and no frontend is nothing.
    if !composition.blocked.is_empty() {
        let blocked = composition.blocked.clone();
        return Composed { composition, log, blocked, changes: None, report: None };
    }

    // 2. Do the halves agree about which interface they built against?
    let winners = composition.winners();
    for (part, entry) in &winners {
        if let Err(e) = w.registry.built_against(&entry.digest, part, composition.contract_version) {
            log.push(format!("could not record what {part} built against: {e}"));
        }
    }
    let blocked = w
        .registry
        .composable(&winners.iter().map(|(_, e)| e.digest.clone()).collect::<Vec<_>>())
        .unwrap_or_else(|e| vec![format!("the registry could not say whether these compose: {e}")]);
    if !blocked.is_empty() {
        return Composed { composition, log, blocked, changes: None, report: None };
    }

    // 3. One tree.
    let changes = match merge(&winners) {
        Ok(c) => c,
        Err(conflicts) => {
            return Composed { composition, log, blocked: conflicts, changes: None, report: None }
        }
    };

    // 4. Two green parts are not a green whole.
    match gate(w.checks_url, base_commit, base_tree, &changes, composition_checks, timeout) {
        Ok(report) => {
            let blocked = if report.passed {
                Vec::new()
            } else {
                let mut why = vec![format!(
                    "the halves pass alone and not together (score {})",
                    report.score
                )];
                why.extend(report.failures.clone());
                why
            };
            Composed { composition, log, blocked, changes: Some(changes), report: Some(report) }
        }
        Err(e) => Composed {
            composition,
            log,
            blocked: vec![format!("the composition gate could not run: {e}")],
            changes: Some(changes),
            report: None,
        },
    }
}
