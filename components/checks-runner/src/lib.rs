//! `checks-runner` — `graph:fitness` over the native check runner.
//!
//! All of the judgement lives here, in a component that can be reasoned about and
//! swapped; all of the process-spawning lives in `comp-checks`, which is native
//! because it must be. The split is the point: a gate has to run the project's
//! own tests, and a component cannot spawn a process — so the only part that
//! needs an operating system is the part that has one, and it is reached over
//! `wasi:http` like everything else.
//!
//! ## The gate and the score come back together
//!
//! The runner reports every check. This turns that into the two answers a caller
//! actually needs, and keeps them apart (ADR-0081): `accepted` is every required
//! check passing, `score` is the weighted fraction of all of them. A binary
//! verdict would throw away the only signal available in the generation where
//! nothing passes yet.
//!
//! ## `need-base` is not a failure
//!
//! The runner caches base trees by commit, and asks for one it has not seen.
//! That comes back as its own error case rather than as a rejection, because a
//! caller answers it by sending the tree — not by concluding the candidate is
//! bad. Collapsing it into a failure would make a cold cache look like a broken
//! branch.

#[allow(warnings)]
mod bindings;

use bindings::exports::graph::fitness::evaluator::{
    Candidate, Check, CheckState, EvalError, Guest, Outcome, Verdict,
};
use bindings::wasi::config::store as config;
use bindings::wasi::http::types::{
    Fields, Method, OutgoingBody, OutgoingRequest, RequestOptions, Scheme,
};

struct Component;

fn cfg(key: &str, default: &str) -> String {
    config::get(key).ok().flatten().filter(|s| !s.is_empty()).unwrap_or_else(|| default.to_string())
}

/// Where the runner is. Split by hand rather than adding a URL crate for one
/// shape of string.
fn endpoint() -> Result<(Scheme, String, String), EvalError> {
    let url = cfg("checks-url", "");
    if url.is_empty() {
        return Err(EvalError::Invalid(
            "checks-url is not set — this evaluator has nowhere to run checks".into(),
        ));
    }
    let (scheme, rest) = match url.split_once("://") {
        Some(("https", r)) => (Scheme::Https, r),
        Some(("http", r)) => (Scheme::Http, r),
        _ => {
            return Err(EvalError::Invalid(format!(
                "checks-url must start with http:// or https://, got {url:?}"
            )))
        }
    };
    let (authority, path) = match rest.split_once('/') {
        Some((a, p)) => (a.to_string(), format!("/{p}")),
        None => (rest.to_string(), "/check".to_string()),
    };
    Ok((scheme, authority, path))
}

fn files_json(files: &[bindings::exports::graph::fitness::evaluator::File]) -> serde_json::Value {
    serde_json::Value::Array(
        files.iter().map(|f| serde_json::json!({ "path": f.path, "content": f.content })).collect(),
    )
}

/// POST the candidate and hand back (status, body).
fn post(body: &str) -> Result<(u16, String), EvalError> {
    let (scheme, authority, path) = endpoint()?;
    let headers = Fields::new();
    let _ = headers.set("content-type", &[b"application/json".to_vec()]);

    let req = OutgoingRequest::new(headers);
    let net = |m: String| EvalError::Unavailable(m);
    req.set_method(&Method::Post).map_err(|_| net("set method".into()))?;
    req.set_scheme(Some(&scheme)).map_err(|_| net("set scheme".into()))?;
    req.set_authority(Some(&authority)).map_err(|_| net("set authority".into()))?;
    req.set_path_with_query(Some(&path)).map_err(|_| net("set path".into()))?;

    let out = req.body().map_err(|_| net("no request body".into()))?;
    {
        let stream = out.write().map_err(|_| net("no request stream".into()))?;
        // A whole base tree goes through here, so it is chunked: the WASI write
        // caps at 4096 bytes a call and a repository is rather larger.
        for chunk in body.as_bytes().chunks(4096) {
            stream
                .blocking_write_and_flush(chunk)
                .map_err(|e| net(format!("writing the candidate: {e:?}")))?;
        }
    }
    OutgoingBody::finish(out, None).map_err(|_| net("finishing the body".into()))?;

    let opts = RequestOptions::new();
    let _ = opts.set_connect_timeout(Some(10_000_000_000));
    // Generous, because the thing on the other end is compiling and running a
    // test suite. A gate that timed out before the tests finished would report
    // every slow candidate as bad.
    let _ = opts.set_first_byte_timeout(Some(900_000_000_000));

    let fut = bindings::wasi::http::outgoing_handler::handle(req, Some(opts))
        .map_err(|e| net(format!("sending: {e:?}")))?;
    fut.subscribe().block();
    let resp = fut
        .get()
        .ok_or_else(|| net("no response".into()))?
        .map_err(|_| net("response already taken".into()))?
        .map_err(|e| net(format!("connecting: {e:?}")))?;

    let status = resp.status();
    let incoming = resp.consume().map_err(|_| net("no response body".into()))?;
    let stream = incoming.stream().map_err(|_| net("no response stream".into()))?;
    let mut buf = Vec::new();
    loop {
        match stream.blocking_read(64 * 1024) {
            Ok(chunk) if chunk.is_empty() => break,
            Ok(chunk) => buf.extend_from_slice(&chunk),
            // End of body.
            Err(bindings::wasi::io::streams::StreamError::Closed) => break,
            // A failed read is not the end of the answer. Keeping the truncated
            // bytes turns half a reply into a whole one that happens to be wrong.
            Err(e) => return Err(net(format!("reading the response: {e:?}"))),
        }
    }
    Ok((status, String::from_utf8_lossy(&buf).into_owned()))
}


/// The checks in the order they may run: one list per LEVEL, and everything in a
/// level is independent of everything else in it.
///
/// Three refusals, all of them `invalid` rather than a failed check, because each
/// is a mistake in the gate itself and no candidate can do anything about it:
///
///   - a CYCLE, named, because a graph that cannot be ordered has no first check;
///   - an unknown id, because a typo that silently means "no dependency" gives you
///     parallelism you did not ask for and a report that lies about why something
///     ran;
///   - a required check that needs an optional one, because that lets a check
///     explicitly marked as not mattering decide whether the gate opens.
///
/// Kahn's algorithm, with the ready set taken a whole level at a time — the level
/// IS the answer to "what can run at once".
fn plan(checks: &[Check]) -> Result<Vec<Vec<usize>>, String> {
    let index: std::collections::BTreeMap<&str, usize> =
        checks.iter().enumerate().map(|(i, c)| (c.id.as_str(), i)).collect();
    if index.len() != checks.len() {
        return Err("two checks share an id — a graph cannot have two of the same node".into());
    }

    for c in checks {
        for need in &c.needs {
            let Some(&at) = index.get(need.as_str()) else {
                return Err(format!("check `{}` needs `{need}`, which no check declares", c.id));
            };
            if c.required && !checks[at].required {
                return Err(format!(
                    "required check `{}` needs `{need}`, which is optional — an optional \
                     check would then decide whether the gate opens",
                    c.id
                ));
            }
        }
    }

    let mut remaining: Vec<usize> = (0..checks.len()).collect();
    let mut done: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();
    let mut levels: Vec<Vec<usize>> = Vec::new();

    while !remaining.is_empty() {
        let ready: Vec<usize> = remaining
            .iter()
            .copied()
            .filter(|&i| checks[i].needs.iter().all(|n| done.contains(&index[n.as_str()])))
            .collect();
        if ready.is_empty() {
            // Whatever is left is in a cycle, or downstream of one. Name the
            // members rather than the abstraction: "there is a cycle" sends the
            // reader back to the file to find it.
            let mut names: Vec<&str> = remaining.iter().map(|&i| checks[i].id.as_str()).collect();
            names.sort();
            return Err(format!("these checks need each other in a cycle: {}", names.join(", ")));
        }
        for &i in &ready {
            done.insert(i);
        }
        remaining.retain(|i| !ready.contains(i));
        levels.push(ready);
    }
    Ok(levels)
}


/// Walk the levels, asking `run` for each level's results.
///
/// Pure apart from `run`, so the blocking rules can be tested without a runner:
/// what gets skipped, what it names as the reason, and what order it all comes
/// back in. `evaluate` supplies the real one, which is an HTTP call.
///
/// A check blocked by something that was ITSELF blocked reports the ROOT. "not
/// attempted because `tests` was not attempted" is a chain the reader has to walk,
/// and the answer is always at the end of it.
fn walk_levels<F, E>(
    checks: &[Check],
    levels: &[Vec<usize>],
    mut run: F,
) -> Result<Vec<Outcome>, E>
where
    F: FnMut(&[&Check]) -> Result<Vec<Outcome>, E>,
{
    let mut outcomes: Vec<Outcome> = Vec::new();
    let mut stopped: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();

    for level in levels {
        let mut runnable: Vec<&Check> = Vec::new();
        for &i in level {
            let k = &checks[i];
            match k.needs.iter().find_map(|n| stopped.get(n).cloned()) {
                Some(root) => {
                    stopped.insert(k.id.clone(), root.clone());
                    outcomes.push(Outcome {
                        id: k.id.clone(),
                        required: k.required,
                        weight: k.weight.max(1),
                        state: CheckState::NotAttempted,
                        blocked_by: root,
                        took_ms: 0,
                        detail: String::new(),
                    });
                }
                None => runnable.push(k),
            }
        }
        if runnable.is_empty() {
            continue;
        }
        for o in run(&runnable)? {
            if o.state != CheckState::Passed {
                // Its own id is the root: whatever depends on it stops HERE.
                stopped.insert(o.id.clone(), o.id.clone());
            }
            outcomes.push(o);
        }
    }

    // Back into the order the caller asked in, so a report reads like the file it
    // was written in rather than like the schedule.
    outcomes.sort_by_key(|o| checks.iter().position(|k| k.id == o.id).unwrap_or(usize::MAX));
    Ok(outcomes)
}

/// One level's report into outcomes.
///
/// The gate and the score are recomputed from these rather than trusted from the
/// wire. The runner already sends both, and taking them on faith would put a
/// caller's acceptance rule in a process it does not control — while recomputing
/// costs a fold over a short list and keeps the rule where it can be tested.
fn outcomes_of(report: &serde_json::Value) -> Vec<Outcome> {
    report["results"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|r| Outcome {
            id: r["id"].as_str().unwrap_or_default().to_string(),
            required: r["required"].as_bool().unwrap_or(false),
            weight: r["weight"].as_u64().unwrap_or(1).max(1) as u32,
            state: if r["passed"].as_bool().unwrap_or(false) {
                CheckState::Passed
            } else {
                CheckState::Failed
            },
            blocked_by: String::new(),
            took_ms: r["took_ms"].as_u64().unwrap_or(0),
            detail: r["detail"].as_str().unwrap_or_default().to_string(),
        })
        .collect()
}

/// The gate and the score, from every outcome.
fn verdict_of(outcomes: Vec<Outcome>) -> Verdict {
    // A required check that was never attempted has not passed. A blocked gate is
    // a closed gate.
    let accepted =
        outcomes.iter().filter(|o| o.required).all(|o| o.state == CheckState::Passed);
    // The denominator is everything ASKED FOR, `not-attempted` included. Dropping
    // skipped checks would let a branch that fails early compete against a smaller
    // denominator than one that runs the whole gate.
    let total: u32 = outcomes.iter().map(|o| o.weight).sum();
    let won: u32 =
        outcomes.iter().filter(|o| o.state == CheckState::Passed).map(|o| o.weight).sum();
    let score = if total == 0 { 0 } else { (won * 1000) / total };
    Verdict { accepted, score, outcomes }
}

impl Guest for Component {
    fn evaluate(c: Candidate, checks: Vec<Check>) -> Result<Verdict, EvalError> {
        if checks.is_empty() {
            // A candidate nothing was asked of would be "accepted" by the
            // arithmetic — vacuously, since no required check failed. Refused
            // instead: an empty check list is a caller mistake, and answering it
            // with a pass is how a swarm accepts everything.
            return Err(EvalError::Invalid(
                "no checks — an empty gate accepts everything, which is never what was meant"
                    .into(),
            ));
        }
        if let Some(bad) = checks.iter().find(|c| c.command.is_empty()) {
            return Err(EvalError::Invalid(format!("check `{}` has no command", bad.id)));
        }

        let levels = plan(&checks).map_err(EvalError::Invalid)?;

        let outcomes = walk_levels(&checks, &levels, |runnable| {
            let body = serde_json::json!({
                "candidate": c.name,
                "base_commit": c.base_commit,
                "base_tree": files_json(&c.base_tree),
                "changes": files_json(&c.changes),
                "checks": runnable
                    .iter()
                    .map(|k| serde_json::json!({
                        "id": k.id,
                        "required": k.required,
                        "weight": k.weight.max(1),
                        "command": k.command,
                    }))
                    .collect::<Vec<_>>(),
            })
            .to_string();

            let (status, text) = post(&body)?;
            let parsed: serde_json::Value = serde_json::from_str(&text)
                .map_err(|e| EvalError::Unavailable(format!("unreadable report: {e}")))?;

            match status {
                200 => Ok(outcomes_of(&parsed)),
                // The runner has not seen this base. Its own case, because a caller
                // answers it by sending the tree rather than by concluding anything
                // about the candidate.
                409 => Err(EvalError::NeedBase(
                    parsed["base_commit"].as_str().unwrap_or_default().to_string(),
                )),
                400 => Err(EvalError::Invalid(
                    parsed["error"].as_str().unwrap_or("the runner refused the request").to_string(),
                )),
                other => Err(EvalError::Unavailable(format!(
                    "the runner answered {other}: {}",
                    parsed["error"]
                        .as_str()
                        .unwrap_or(&text.chars().take(200).collect::<String>())
                ))),
            }
        })?;

        Ok(verdict_of(outcomes))
    }
}

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::*;

    fn outcome(id: &str, required: bool, weight: u32, state: CheckState) -> Outcome {
        Outcome {
            id: id.into(),
            required,
            weight,
            state,
            blocked_by: String::new(),
            took_ms: 0,
            detail: String::new(),
        }
    }
    fn passed(id: &str, required: bool, weight: u32) -> Outcome {
        outcome(id, required, weight, CheckState::Passed)
    }
    fn failed(id: &str, required: bool, weight: u32) -> Outcome {
        outcome(id, required, weight, CheckState::Failed)
    }
    fn check(id: &str, required: bool, needs: &[&str]) -> Check {
        Check {
            id: id.into(),
            required,
            weight: 1,
            command: vec!["true".into()],
            needs: needs.iter().map(|s| s.to_string()).collect(),
        }
    }
    fn ids(levels: &[Vec<usize>], checks: &[Check]) -> Vec<Vec<String>> {
        levels
            .iter()
            .map(|l| {
                let mut v: Vec<String> = l.iter().map(|&i| checks[i].id.clone()).collect();
                v.sort();
                v
            })
            .collect()
    }

    // ---- the gate and the score --------------------------------------------

    /// The gate is the required checks, and nothing else gets a vote.
    #[test]
    fn an_optional_check_cannot_close_the_gate() {
        let v = verdict_of(vec![passed("compiles", true, 1), failed("lints", false, 1)]);
        assert!(v.accepted, "a failing optional check must not reject the candidate");
        assert_eq!(v.score, 500, "but it must still cost score");
    }

    #[test]
    fn a_failing_required_check_closes_it() {
        let v = verdict_of(vec![failed("compiles", true, 1), passed("lints", false, 1)]);
        assert!(!v.accepted);
    }

    /// The property the whole contract exists for: something to select on before
    /// anything is acceptable.
    #[test]
    fn the_score_orders_candidates_that_all_fail_the_gate() {
        let poor = verdict_of(vec![
            passed("a", true, 1),
            failed("b", true, 1),
            failed("c", true, 1),
        ]);
        let better = verdict_of(vec![
            passed("a", true, 1),
            passed("b", true, 1),
            failed("c", true, 1),
        ]);
        assert!(!poor.accepted && !better.accepted, "neither may be accepted");
        assert!(
            better.score > poor.score,
            "2 of 3 must beat 1 of 3 while both fail — that ordering is the only signal a \
             search has in its first generation: {} vs {}",
            better.score,
            poor.score
        );
    }

    #[test]
    fn weight_moves_the_score_and_never_the_gate() {
        let v = verdict_of(vec![passed("big", false, 9), failed("small", true, 1)]);
        assert_eq!(v.score, 900, "the heavy check carries the score");
        assert!(!v.accepted, "and the light REQUIRED one still closes the gate");
    }

    // ---- the graph ----------------------------------------------------------

    /// A check that was never attempted has not passed, so a blocked gate is shut.
    #[test]
    fn a_required_check_that_never_ran_closes_the_gate() {
        let v = verdict_of(vec![
            failed("compiles", true, 1),
            outcome("tests", true, 1, CheckState::NotAttempted),
        ]);
        assert!(!v.accepted, "nobody proved `tests` passes, so it did not");
    }

    /// THE scoring trap. Skipped checks stay in the denominator, or failing early
    /// competes against a smaller one — which pays a search to break the build.
    #[test]
    fn skipping_a_check_never_raises_the_score() {
        let ran_everything = verdict_of(vec![
            passed("compiles", true, 1),
            passed("tests", true, 1),
            failed("bench", false, 1),
        ]);
        let failed_early = verdict_of(vec![
            passed("compiles", true, 1),
            failed("tests", true, 1),
            outcome("bench", false, 1, CheckState::NotAttempted),
        ]);
        assert_eq!(ran_everything.score, 666);
        assert_eq!(failed_early.score, 333);
        assert!(
            failed_early.score < ran_everything.score,
            "a branch that stopped early must never outscore one that went further"
        );
    }

    /// A flat list is a graph with one level, so every gate written before the
    /// edges existed behaves exactly as it did.
    #[test]
    fn no_edges_is_one_level() {
        let checks = vec![check("a", true, &[]), check("b", true, &[]), check("c", false, &[])];
        let levels = plan(&checks).expect("a flat list always plans");
        assert_eq!(levels.len(), 1);
        assert_eq!(ids(&levels, &checks), vec![vec!["a", "b", "c"]]);
    }

    /// And the levels are what may run at once: `lints` and `tests` both need
    /// `compiles` and neither needs the other.
    #[test]
    fn the_levels_are_what_can_run_together() {
        let checks = vec![
            check("bench", false, &["tests"]),
            check("compiles", true, &[]),
            check("lints", false, &["compiles"]),
            check("tests", true, &["compiles"]),
        ];
        assert_eq!(
            ids(&plan(&checks).expect("plans"), &checks),
            vec![vec!["compiles"], vec!["lints", "tests"], vec!["bench"]]
        );
    }

    #[test]
    fn a_cycle_is_refused_and_named() {
        let checks = vec![check("a", true, &["c"]), check("b", true, &["a"]), check("c", true, &["b"])];
        let e = plan(&checks).expect_err("a cycle has no first check");
        assert!(e.contains("cycle"), "{e}");
        for id in ["a", "b", "c"] {
            assert!(e.contains(id), "the members, not just the abstraction: {e}");
        }
    }

    /// A typo that silently meant "no dependency" would give parallelism nobody
    /// asked for and a report that lies about why something ran.
    #[test]
    fn a_dependency_nothing_declares_is_refused() {
        let checks = vec![check("tests", true, &["compile"])];
        let e = plan(&checks).expect_err("`compile` is not `compiles`");
        assert!(e.contains("compile") && e.contains("tests"), "{e}");
    }

    /// The subtle one: an optional check must not decide whether the gate opens.
    #[test]
    fn a_required_check_may_not_hang_off_an_optional_one() {
        let checks = vec![check("lints", false, &[]), check("tests", true, &["lints"])];
        let e = plan(&checks).expect_err("that lets `lints` close the gate");
        assert!(e.contains("optional"), "{e}");

        // The other way round is fine: an optional check may wait on a required one.
        let ok = vec![check("compiles", true, &[]), check("bench", false, &["compiles"])];
        assert!(plan(&ok).is_ok());
    }

    // ---- the walk -----------------------------------------------------------

    /// Run the graph with a scripted runner: every check passes unless it is named
    /// in `fails`. No HTTP, so what is under test is the blocking, not the wire.
    fn walk(checks: &[Check], fails: &[&str]) -> Vec<Outcome> {
        let levels = plan(checks).expect("plans");
        walk_levels::<_, ()>(checks, &levels, |runnable| {
            Ok(runnable
                .iter()
                .map(|k| {
                    let bad = fails.contains(&k.id.as_str());
                    Outcome {
                        id: k.id.clone(),
                        required: k.required,
                        weight: k.weight.max(1),
                        state: if bad { CheckState::Failed } else { CheckState::Passed },
                        blocked_by: String::new(),
                        took_ms: 1,
                        detail: if bad { "boom".into() } else { String::new() },
                    }
                })
                .collect())
        })
        .expect("no runner error")
    }

    fn gate() -> Vec<Check> {
        vec![
            check("compiles", true, &[]),
            check("tests", true, &["compiles"]),
            check("lints", false, &["compiles"]),
            check("bench", false, &["tests"]),
        ]
    }

    /// THE case the graph exists for. A candidate that does not compile used to
    /// arrive as four failures and four walls of output; it arrives as ONE failure
    /// and three things nobody tried.
    #[test]
    fn a_compile_failure_produces_one_failure_and_a_list_of_untried_things() {
        let checks = gate();
        let outcomes = walk(&checks, &["compiles"]);

        let failed: Vec<&str> = outcomes
            .iter()
            .filter(|o| o.state == CheckState::Failed)
            .map(|o| o.id.as_str())
            .collect();
        assert_eq!(failed, vec!["compiles"], "exactly one thing is wrong");

        let untried: Vec<(&str, &str)> = outcomes
            .iter()
            .filter(|o| o.state == CheckState::NotAttempted)
            .map(|o| (o.id.as_str(), o.blocked_by.as_str()))
            .collect();
        assert_eq!(
            untried,
            vec![("tests", "compiles"), ("lints", "compiles"), ("bench", "compiles")],
            "and every one of them names the ROOT, not the link above it"
        );
    }

    /// `bench` needs `tests` which needs `compiles`. It must not report `tests` —
    /// a chain the reader has to walk always ends at the same place.
    #[test]
    fn a_blocked_check_names_the_root_and_not_the_link() {
        let outcomes = walk(&gate(), &["compiles"]);
        let bench = outcomes.iter().find(|o| o.id == "bench").unwrap();
        assert_eq!(bench.blocked_by, "compiles", "not `tests`, which never ran either");
    }

    /// A failure only stops what depends on it. `lints` has nothing to do with
    /// `tests` and must still run.
    #[test]
    fn a_failure_blocks_its_dependents_and_nothing_else() {
        let outcomes = walk(&gate(), &["tests"]);
        let state = |id: &str| outcomes.iter().find(|o| o.id == id).unwrap().state;
        assert_eq!(state("compiles"), CheckState::Passed);
        assert_eq!(state("tests"), CheckState::Failed);
        assert_eq!(state("lints"), CheckState::Passed, "lints does not need tests");
        assert_eq!(state("bench"), CheckState::NotAttempted, "bench does");
    }

    /// The report comes back in the order the gate was WRITTEN, not the order the
    /// schedule happened to run it.
    #[test]
    fn the_report_reads_like_the_file_it_was_written_in() {
        let checks = vec![
            check("bench", false, &["tests"]),
            check("compiles", true, &[]),
            check("tests", true, &["compiles"]),
        ];
        let outcomes = walk(&checks, &[]);
        let ids: Vec<&str> = outcomes.iter().map(|o| o.id.as_str()).collect();
        assert_eq!(ids, vec!["bench", "compiles", "tests"]);
    }

    /// Everything green is still everything green.
    #[test]
    fn a_passing_graph_runs_all_of_it() {
        let outcomes = walk(&gate(), &[]);
        assert!(outcomes.iter().all(|o| o.state == CheckState::Passed));
        let v = verdict_of(outcomes);
        assert!(v.accepted);
        assert_eq!(v.score, 1000);
    }

    #[test]
    fn two_checks_cannot_share_an_id() {
        let checks = vec![check("a", true, &[]), check("a", true, &[])];
        assert!(plan(&checks).is_err());
    }
}
