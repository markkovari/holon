//! Joining the parts of a decomposed goal: the mocks that let them diverge, the
//! merge that puts them back together, and the gate that judges the whole.
//!
//! Two green parts are not a green whole (ADR-0086). Each part gates alone and neither waits for the
//! other; this module merges the winners, runs the goal's own checks over the joined tree, and
//! refuses loudly when the halves were built against different contract versions.
//!
//! The gate here is **the same runner** the branches were judged by: a composition judged by
//! different machinery is a composition whose failures are arguments about the harness.

use std::time::Duration;

use serde_json::{json, Value};

use crate::generation::Entry;

/// Where fixtures generated from the contract are laid in a part's tree.
pub const MOCK_DIR: &str = ".contract-mocks";

/// How a part asks for a change to the contract.
///
/// A model has no tool and giving it the registry would mean an agent reaching the shared interface
/// directly, so a request rides the channel a branch already has: **a file in its candidate**. First
/// line is the subject, the rest the body; the orchestrator turns it into an `ask` and drops it, as
/// merging it would land a question in the pull request.
pub const REQUEST_PATH: &str = "CONTRACT-REQUEST.md";

/// The request a candidate is carrying, if any.
pub fn request_of(entry: &Entry) -> Option<(String, String)> {
    let files = entry.files.as_array()?;
    let f = files.iter().find(|f| f["path"] == json!(REQUEST_PATH))?;
    let content = f["content"].as_str()?.trim();
    let (subject, body) = match content.split_once('\n') {
        // A model writing a markdown file writes a markdown TITLE, so the first line comes back as
        // `# Contract Request` and every request dedups onto that one subject.
        Some((s, b)) => (s.trim().trim_start_matches('#').trim(), b.trim()),
        // A subject with no body is a question with no argument; allowed, not worth failing a round over.
        None => (content, ""),
    };
    if subject.is_empty() {
        return None;
    }
    Some((subject.to_string(), body.to_string()))
}

/// Every distinct request in a generation's worth of candidates.
///
/// Read from ALL entries, not the winner: a branch that scored zero may be the one that noticed the
/// interface is wrong, and when no branch passes there is no winner, the round a part most needs to ask.
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
/// "Mock generation" in the only form that costs nothing and works in every language: **files**, not a
/// server (which needs a runtime per stack, a port and a keeper). The part's tree is already how a
/// branch is handed everything it knows.
///
/// The contract's own `example` is the fixture. A route without one produces no file: inventing one
/// would put a shape in the frontend's tests the backend never agreed to.
pub fn mocks(contract: &str) -> Vec<(String, String)> {
    let Ok(v) = serde_json::from_str::<Value>(contract) else {
        // A contract that is not JSON cannot be read, and a fixture guessed from prose is worse than none.
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
/// Two parts writing the same path is refused rather than resolved: their `writable` sets should be
/// disjoint, so an overlap is a decomposition bug and taking one side would land half of each.
pub fn merge(winners: &[(String, Entry)]) -> Result<Value, Vec<String>> {
    let mut merged: Vec<Value> = Vec::new();
    let mut owner: Vec<(String, String)> = Vec::new();
    let mut conflicts = Vec::new();
    for (part, entry) in winners {
        for f in entry.files.as_array().cloned().unwrap_or_default() {
            let path = f["path"].as_str().unwrap_or_default().to_string();
            // A request is a question, not code, and landing it would put a question in the pull request.
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

#[derive(Debug, Clone)]
pub struct Report {
    pub passed: bool,
    pub score: u64,
    pub failures: Vec<String>,
}

/// Run the goal's own checks over the joined tree, on the same runner that judged the parts.
pub fn gate(
    checks_url: &str,
    // `checks_token` is the runner's bearer token, when it wants one; `None` is a supported way to run.
    checks_token: Option<&str>,
    base_commit: &str,
    base_tree: &Value,
    changes: &Value,
    checks: &Value,
    timeout: Duration,
) -> Result<Report, String> {
    if checks.as_array().map(|c| c.is_empty()).unwrap_or(true) {
        // The same refusal the evaluator makes: an empty gate accepts everything, never what was meant.
        return Err("no composition checks — an empty gate would accept any two halves".into());
    }
    let body = json!({
        "candidate": "composition",
        "base_commit": base_commit,
        "base_tree": base_tree,
        "changes": changes,
        "checks": checks,
    });
    let mut req = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| e.to_string())?
        .post(checks_url)
        .body(body.to_string());
    if let Some(t) = checks_token {
        req = req.bearer_auth(t);
    }
    let r = req.send().map_err(|e| format!("{e}"))?;
    let text = r.text().unwrap_or_default();
    let v: Value =
        serde_json::from_str(&text).map_err(|e| format!("unreadable report ({e}): {text}"))?;
    Ok(report_of(&v))
}

/// The runner's report, read the way the evaluator reads it.
///
/// Two field names captured from `comp-checks`: the per-check list is **`results`**, the gate is
/// **`accepted`**. `passed` at the top level is a COUNT, so reading it as a boolean fails every
/// composition with an empty list of reasons.
pub fn report_of(v: &Value) -> Report {
    let outcomes = v["results"].as_array().cloned().unwrap_or_default();
    let failures: Vec<String> = outcomes
        .iter()
        .filter(|o| o["passed"] != json!(true))
        .map(|o| {
            format!(
                "{}: {}",
                o["id"].as_str().unwrap_or("?"),
                o["detail"]
                    .as_str()
                    .unwrap_or("failed")
                    .lines()
                    .take(3)
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        })
        .collect();
    Report {
        // The runner has already decided the gate; trusting it keeps the gate in one place. `failures` is
        // belt and braces for a report that says accepted with a required check missing.
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
        let v1 =
            with_mocks(&plan, r#"{"routes":[{"method":"GET","path":"/a","example":{"v":1}}]}"#);
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
        assert_eq!(
            subject, "SearchResult needs total_pages",
            "a markdown heading is not a subject"
        );
        assert!(body.starts_with("I cannot paginate"));

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
        // An empty request file is not a request.
        assert!(request_of(&entry(json!([{ "path": REQUEST_PATH, "content": "  \n " }]))).is_none());
    }

    #[test]
    fn a_check_that_cannot_fail_is_refused_and_an_excused_one_is_not() {
        let vacuous = vec![
            Vacuous { id: "component-compiles".into(), excused: false },
            Vacuous { id: "no-regression".into(), excused: true },
        ];
        let out = refusal(&vacuous);
        assert_eq!(out.len(), 1, "only the unexcused one is a refusal: {out:?}");
        assert!(
            out[0].contains("`component-compiles` already passes on the base tree"),
            "{}",
            out[0]
        );
        // The message has to say what to do, or the critic gets disabled rather than fixed.
        assert!(out[0].contains("Make it fail against the code as it stands"), "{}", out[0]);
        assert!(out[0].contains("may_pass_base = true"), "{}", out[0]);
        assert!(refusal(&[]).is_empty());
    }

    #[test]
    fn criticising_no_checks_is_itself_a_refusal() {
        let e = criticise(
            "http://127.0.0.1:1",
            None,
            "c",
            &json!([]),
            &json!([]),
            &[],
            Duration::from_secs(1),
        )
        .expect_err("an empty gate accepts everything");
        assert!(e.contains("empty gate"), "{e}");
    }

    #[test]
    fn an_empty_gate_is_refused_rather_than_passed() {
        let e = gate(
            "http://127.0.0.1:1",
            None,
            "c",
            &json!([]),
            &json!([]),
            &json!([]),
            Duration::from_secs(1),
        )
        .expect_err("an empty gate accepts everything");
        assert!(e.contains("empty gate"), "{e}");
    }

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

// The whole decomposed run, in one place.
// This lived in `comp-goalrun` and again in the e2e meant to cover it, so the test exercised a
// re-spelling of the binary; two copies drift. The loop is library code with a thin caller: the
// binary prints and lands, the test asserts, both drive THIS.

use std::cell::RefCell;
use std::collections::BTreeMap;

use crate::contract::{Answerer, Ask, Registry};
use crate::generation::{
    compose_search, default_strategies, Bounds, Composition, Part, PartOutcome, Strategy,
};
use crate::memory::{self, Memory, Reading};

pub struct Composed {
    pub composition: Composition,
    /// Every negotiation line, in order: the part a reviewer most needs and could never reconstruct.
    pub log: Vec<String>,
    /// Why there is nothing to land, or empty: a part that never passed, halves on different contract
    /// versions, two parts writing one path, a failed join. Each must name itself.
    pub blocked: Vec<String>,
    pub changes: Option<Value>,
    pub report: Option<Report>,
}

impl Composed {
    pub fn landable(&self) -> bool {
        self.blocked.is_empty()
            && self.changes.is_some()
            && self.report.as_ref().map(|r| r.passed).unwrap_or(false)
    }
}

pub struct Wiring<'a> {
    pub driver_url: &'a str,
    pub driver_host: &'a str,
    pub checks_url: &'a str,
    /// The token the COMPOSITION gate presents. The per-part gates go through `checks-runner`, which reads
    /// its own from `comp:secrets`; this is the one call the loop makes directly, and without the same
    /// credential the composition is 401 while every part was fine.
    pub checks_token: Option<&'a str>,
    pub registry: &'a Registry,
    /// `None` runs the loop without answering anything: requests accumulate, the run continues on the
    /// current contract, nothing blocks.
    pub answerer: Option<&'a Answerer>,
    /// `None` runs the parts with no memory: they read nothing and write nothing.
    pub memory: Option<&'a Memory>,
}

/// Run a decomposed goal to the point where a pull request could be opened.
///
/// Does no printing and opens nothing: what a run should SAY and whether it lands are the caller's,
/// and doing both would need a forge to test.
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
    let log = RefCell::new(Vec::<String>::new());
    let readings: RefCell<BTreeMap<(String, usize), Vec<String>>> = RefCell::new(BTreeMap::new());

    // What a round taught, and what it owed. Called at every boundary AND once after the loop: the loop
    // breaks when every part passes, so the boundary never sees the round that succeeded.
    let learn = |outcomes: &[PartOutcome]| {
        let Some(m) = w.memory else { return };
        for o in outcomes {
            let Some(round) = o.rounds.last() else { continue };
            for (i, e) in round.entries.iter().enumerate() {
                if !e.accepted {
                    if let Some(text) = memory::failure_text(&e.failures, e.score) {
                        if let Err(err) = m.observe_failure(&o.part, &o.part, &e.branch, &text) {
                            log.borrow_mut()
                                .push(format!("{}: lesson not recorded: {err}", o.part));
                        }
                    }
                }
                let keys = readings.borrow().get(&(o.part.clone(), i)).cloned();
                if let Some(keys) = keys {
                    let run = format!("{}/{}", o.part, e.branch);
                    if let Err(err) = m.attribute(&keys, &run, e.accepted) {
                        log.borrow_mut().push(format!("{}: reading not attributed: {err}", o.part));
                    }
                }
            }
        }
    };

    let composition = compose_search(
        w.driver_url,
        w.driver_host,
        parts,
        contract,
        version,
        bounds,
        seed,
        timeout,
        // What each part reads, per round. A part asks about ITS OWN goal: a backend and a frontend attempting
        // one feature want different lessons, and the pool is keyed by the goal asked.
        |part: &Part, _round: u16| -> Vec<Strategy> {
            let mut ss = default_strategies(bounds.branches);
            let Some(m) = w.memory else { return ss };
            let goal = part.plan["text"].as_str().unwrap_or_default().to_string();
            // What this part's work touches, so what it learns is findable by the next part to build against the
            // same interfaces (ADR-0090).
            let writable: Vec<String> = part.plan["writable"]
                .as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect())
                .unwrap_or_default();
            let tags = comp_fleet::plug::tags_for(
                &writable,
                &comp_fleet::plug::Catalog::scan(&comp_fleet::plug::default_dirs(
                    &comp_fleet::fleet::repo_root(),
                )),
            );
            for (i, s) in ss.iter_mut().enumerate() {
                // The control arm reads nothing here too: the only way to tell whether the pool helps this part.
                if !s.reads_prior {
                    continue;
                }
                let reading = Reading {
                    k: 3 + (i as u32 % 3),
                    budget: 1200,
                    pools: match i % 3 {
                        0 => vec![],
                        1 => vec!["errors".into()],
                        _ => vec!["patterns".into(), "solutions".into()],
                    },
                    tags: tags.clone(),
                    min_similarity: 0.0,
                };
                match m.recall(&goal, &reading) {
                    Ok(lessons) if lessons.is_empty() => {}
                    Ok(lessons) => {
                        readings.borrow_mut().insert(
                            (part.name.clone(), i),
                            lessons.iter().map(|l| l.key.clone()).collect(),
                        );
                        s.knowledge = memory::render(&lessons);
                    }
                    Err(e) => {
                        log.borrow_mut().push(format!("{} branch-{i} runs cold: {e}", part.name))
                    }
                }
            }
            ss
        },
        |round, outcomes| {
            learn(outcomes);
            // A part asks by writing a file and the orchestrator turns it into a request: a model has no tool.
            for o in outcomes {
                let Some(last) = o.rounds.last() else { continue };
                for (subject, body) in requests_in(&last.entries) {
                    // Addressed to the other part. With more than two this needs a routing rule.
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
                        Ok(_) => log.borrow_mut().push(format!(
                            "generation {round}: {} asked {to} for {subject:?}",
                            o.part
                        )),
                        Err(e) => {
                            log.borrow_mut().push(format!("generation {round}: could not ask: {e}"))
                        }
                    }
                }
            }
            let mut lines = log.borrow_mut();
            match w.registry.boundary(outcomes, w.answerer, &mut lines) {
                Ok(next) => next,
                // A boundary that cannot say what the contract is has nothing to hand the next round, and carrying on
                // from a guess is how two passing halves fail together. The round repeats on what each part had.
                Err(e) => {
                    lines.push(format!("generation {round}: the boundary failed: {e}"));
                    Vec::new()
                }
            }
        },
    );

    learn(&composition.parts);
    let mut log = log.into_inner();

    // 1. Every part, or nothing.
    if !composition.blocked.is_empty() {
        let blocked = composition.blocked.clone();
        return Composed { composition, log, blocked, changes: None, report: None };
    }

    // 2. Do the halves agree about which interface they built against?
    let winners = composition.winners();
    for (part, entry) in &winners {
        if let Err(e) = w.registry.built_against(&entry.digest, part, composition.contract_version)
        {
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
    match gate(
        w.checks_url,
        w.checks_token,
        base_commit,
        base_tree,
        &changes,
        composition_checks,
        timeout,
    ) {
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

// Criticising the gate (goal 07).

/// A check that already passes on the UNMODIFIED base tree.
///
/// Such a check cannot fail, so it cannot judge: a run gated on it accepts anything. The first real
/// decomposed run scored 1000 on two candidates that had deleted their own component exports, because
/// `cargo component check` passes on a crate that implements none of its world. The critic runs
/// BEFORE the money is spent.
#[derive(Debug, Clone)]
pub struct Vacuous {
    pub id: String,
    pub excused: bool,
}

/// others failed.
///
/// The reasons are carried out because "every check fails" is two states: the work is not done (a gate
/// that can judge) or the tree cannot even build (a gate that fails every candidate identically yet
/// reads as healthy; this cost run 1 of docs/measure/complex-five.md its whole budget, 36 model calls).
#[derive(Debug)]
pub struct BaseVerdict {
    pub vacuous: Vec<Vacuous>,
    pub reasons: Vec<String>,
}

/// Put the base tree in the runner's cache, and confirm it landed.
///
/// Once the runner has the tree keyed by commit nothing downstream carries it: a branch's plan names
/// the commit only, so 500 KB stops crossing the lattice per generation and the message ceiling stops
/// limiting a goal's tree. A commit id is a content address, so no invalidation logic is needed.
///
/// Sent with NO checks, because this is not a judgement: `comp-checks` caches the tree in `ensure_base`
/// before it looks at the check list, so an empty list materialises the base and runs nothing.
///
/// Failure is fatal to the caller on purpose: a run that proceeded would have an unseeded runner and
/// no tree in the plan to fix it with.
pub fn seed_base(
    checks_url: &str,
    checks_token: Option<&str>,
    base_commit: &str,
    base_tree: &Value,
    timeout: Duration,
) -> Result<(), String> {
    if base_commit.is_empty() {
        return Err("no base commit — the runner keys its cache by one".into());
    }
    let body = json!({
        "candidate": "seed",
        "base_commit": base_commit,
        "base_tree": base_tree,
        "changes": [],
        "checks": [],
    });
    let mut req = reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| e.to_string())?
        .post(checks_url)
        .body(body.to_string());
    if let Some(t) = checks_token {
        req = req.bearer_auth(t);
    }
    let r = req.send().map_err(|e| format!("{e}"))?;
    let status = r.status().as_u16();
    let text = r.text().unwrap_or_default();
    if status != 200 {
        // 409 here means the runner did not accept the tree it was just handed, a different fault from a cold
        // cache.
        return Err(format!(
            "the runner would not take the base tree ({status}): {}",
            text.chars().take(300).collect::<String>()
        ));
    }
    Ok(())
}

/// Run the checks against the base tree alone and report the ones that pass.
///
/// `excuse` names checks a caller declared may legitimately pass on the base (a regression test, a
/// benchmark that must not get slower); refusing them would make the critic something people turn off.
pub fn criticise(
    checks_url: &str,
    checks_token: Option<&str>,
    base_commit: &str,
    base_tree: &Value,
    checks: &Value,
    excuse: &[String],
    timeout: Duration,
) -> Result<BaseVerdict, String> {
    let list = checks.as_array().cloned().unwrap_or_default();
    if list.is_empty() {
        return Err("no checks to criticise — an empty gate accepts everything".into());
    }
    // The same runner, the same tree, and NO changes: anything green here is green for a candidate that
    // did nothing.
    let report =
        gate(checks_url, checks_token, base_commit, base_tree, &json!([]), checks, timeout)?;
    let failed: Vec<&str> = report.failures.iter().filter_map(|f| f.split(':').next()).collect();
    Ok(BaseVerdict {
        vacuous: list
            .iter()
            .filter_map(|c| c["id"].as_str())
            .filter(|id| !failed.contains(id))
            .map(|id| Vacuous { id: id.to_string(), excused: excuse.iter().any(|e| e == id) })
            .collect(),
        reasons: report.failures.clone(),
    })
}

pub fn refusal(vacuous: &[Vacuous]) -> Vec<String> {
    vacuous
        .iter()
        .filter(|v| !v.excused)
        .map(|v| {
            format!(
                "`{}` already passes on the base tree, so it cannot judge a candidate. \
                 Make it fail against the code as it stands, or mark it `may_pass_base = true` \
                 if it is a regression check that is supposed to be green.",
                v.id
            )
        })
        .collect()
}
