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
    Candidate, Check, EvalError, Guest, Outcome, Verdict,
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
        files
            .iter()
            .map(|f| serde_json::json!({ "path": f.path, "content": f.content }))
            .collect(),
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

/// Turn the runner's report into a verdict.
///
/// The gate and the score are recomputed here rather than trusted from the wire.
/// The runner already sends both, and taking them on faith would mean a caller's
/// acceptance rule lived in a process it does not control — while recomputing
/// costs a fold over a short list and keeps the rule where it can be tested.
fn verdict_of(report: &serde_json::Value) -> Verdict {
    let outcomes: Vec<Outcome> = report["results"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|r| Outcome {
            id: r["id"].as_str().unwrap_or_default().to_string(),
            required: r["required"].as_bool().unwrap_or(false),
            weight: r["weight"].as_u64().unwrap_or(1).max(1) as u32,
            passed: r["passed"].as_bool().unwrap_or(false),
            took_ms: r["took_ms"].as_u64().unwrap_or(0),
            detail: r["detail"].as_str().unwrap_or_default().to_string(),
        })
        .collect();

    let accepted = outcomes.iter().filter(|o| o.required).all(|o| o.passed);
    let total: u32 = outcomes.iter().map(|o| o.weight).sum();
    let won: u32 = outcomes.iter().filter(|o| o.passed).map(|o| o.weight).sum();
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

        let body = serde_json::json!({
            "candidate": c.name,
            "base_commit": c.base_commit,
            "base_tree": files_json(&c.base_tree),
            "changes": files_json(&c.changes),
            "checks": checks
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
            200 => Ok(verdict_of(&parsed)),
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
                parsed["error"].as_str().unwrap_or(&text.chars().take(200).collect::<String>())
            ))),
        }
    }
}

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::*;

    fn report(results: serde_json::Value) -> serde_json::Value {
        serde_json::json!({ "results": results })
    }

    /// The gate is the required checks, and nothing else gets a vote.
    #[test]
    fn an_optional_check_cannot_close_the_gate() {
        let v = verdict_of(&report(serde_json::json!([
            { "id": "compiles", "required": true, "weight": 1, "passed": true },
            { "id": "lints", "required": false, "weight": 1, "passed": false },
        ])));
        assert!(v.accepted, "a failing optional check must not reject the candidate");
        assert_eq!(v.score, 500, "but it must still cost score");
    }

    #[test]
    fn a_failing_required_check_closes_it() {
        let v = verdict_of(&report(serde_json::json!([
            { "id": "compiles", "required": true, "weight": 1, "passed": false },
            { "id": "lints", "required": false, "weight": 1, "passed": true },
        ])));
        assert!(!v.accepted);
    }

    /// The property the whole contract exists for: something to select on before
    /// anything is acceptable.
    #[test]
    fn the_score_orders_candidates_that_all_fail_the_gate() {
        let poor = verdict_of(&report(serde_json::json!([
            { "id": "a", "required": true, "weight": 1, "passed": true },
            { "id": "b", "required": true, "weight": 1, "passed": false },
            { "id": "c", "required": true, "weight": 1, "passed": false },
        ])));
        let better = verdict_of(&report(serde_json::json!([
            { "id": "a", "required": true, "weight": 1, "passed": true },
            { "id": "b", "required": true, "weight": 1, "passed": true },
            { "id": "c", "required": true, "weight": 1, "passed": false },
        ])));
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
        let v = verdict_of(&report(serde_json::json!([
            { "id": "big", "required": false, "weight": 9, "passed": true },
            { "id": "small", "required": true, "weight": 1, "passed": false },
        ])));
        assert_eq!(v.score, 900, "the heavy check carries the score");
        assert!(!v.accepted, "and the light REQUIRED one still closes the gate");
    }

    /// A weight of zero would divide by zero or silently vanish; it is read as 1.
    #[test]
    fn a_zero_weight_is_read_as_one() {
        let v = verdict_of(&report(serde_json::json!([
            { "id": "a", "required": false, "weight": 0, "passed": true },
            { "id": "b", "required": false, "weight": 0, "passed": false },
        ])));
        assert_eq!(v.score, 500, "two zero-weight checks are two equal checks");
    }

    #[test]
    fn an_empty_report_scores_zero_rather_than_dividing_by_it() {
        let v = verdict_of(&report(serde_json::json!([])));
        assert_eq!(v.score, 0);
        assert!(v.accepted, "nothing was required, so nothing failed");
    }
}
