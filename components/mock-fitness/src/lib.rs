//! `mock-fitness` — `graph:fitness`, scored by a scripted rule (see wit/fitness.wit).
//!
//! The real gate runs commands and needs egress the platform gives only a
//! graph's front door. This one reaches nothing: it scores a candidate from a
//! `gate-script` config value the same way `mock-provider` scripts a completion,
//! so a whole driver loop can be deployed as a linked graph with no egress at all
//! and driven one branch per environment.
//!
//! ## The script
//!
//! `gate-script` is JSON: for each check id, the substring a candidate's changed
//! files must contain to pass it.
//!
//! ```json
//! { "has-two": "step_two", "be-42": "42" }
//! ```
//!
//! A check whose id is not in the map fails — silence is a fail, not a pass, for
//! the same reason the real evaluator refuses an empty gate: a gate that passes
//! what it was not told about is how a swarm accepts everything.

#[allow(warnings)]
mod bindings;

use bindings::exports::graph::fitness::evaluator::{
    Candidate, Check, EvalError, Guest, Outcome, Verdict,
};
use bindings::wasi::config::store as config;

struct Component;

fn script() -> serde_json::Value {
    let raw = config::get("gate-script").ok().flatten().unwrap_or_default();
    serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null)
}

/// Everything the candidate would write, concatenated — what a rule matches on.
/// The base tree is deliberately NOT included: a scripted gate judges the change,
/// and matching the base would pass a candidate for what it inherited.
fn changed_text(c: &Candidate) -> String {
    c.changes.iter().map(|f| f.content.clone()).collect::<Vec<_>>().join("\n")
}

impl Guest for Component {
    fn evaluate(c: Candidate, checks: Vec<Check>) -> Result<Verdict, EvalError> {
        if checks.is_empty() {
            return Err(EvalError::Invalid("no checks — an empty gate accepts everything".into()));
        }
        let script = script();
        let text = changed_text(&c);

        let outcomes: Vec<Outcome> = checks
            .iter()
            .map(|k| {
                // A check with no rule in the script fails. Not passes: an
                // unscripted gate that waved everything through would make this a
                // stub that proves the loop RUNS by proving nothing about it.
                let passed =
                    script[&k.id].as_str().map(|needle| text.contains(needle)).unwrap_or(false);
                Outcome {
                    id: k.id.clone(),
                    required: k.required,
                    weight: k.weight.max(1),
                    passed,
                    took_ms: 0,
                    detail: if passed { String::new() } else { format!("missing: {}", k.id) },
                }
            })
            .collect();

        // The same arithmetic the real evaluator recomputes, and for the same
        // reason: the gate is every required check, the score is the weighted
        // fraction of all of them.
        let accepted = outcomes.iter().filter(|o| o.required).all(|o| o.passed);
        let total: u32 = outcomes.iter().map(|o| o.weight).sum();
        let won: u32 = outcomes.iter().filter(|o| o.passed).map(|o| o.weight).sum();
        let score = if total == 0 { 0 } else { (won * 1000) / total };

        Ok(Verdict { accepted, score, outcomes })
    }
}

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::*;
    use bindings::exports::graph::fitness::evaluator::File;

    fn candidate(content: &str) -> Candidate {
        Candidate {
            name: "c".into(),
            base_commit: "0".into(),
            base_tree: vec![],
            changes: vec![File { path: "src/lib.rs".into(), content: content.into() }],
        }
    }

    fn check(id: &str, required: bool) -> Check {
        Check { id: id.into(), required, weight: 1, command: vec![] }
    }

    fn with_script(script: &str, c: Candidate, checks: Vec<Check>) -> Verdict {
        // `evaluate` reads config, which a unit test cannot set, so exercise the
        // arithmetic directly against a parsed script.
        let script: serde_json::Value = serde_json::from_str(script).unwrap();
        let text = changed_text(&c);
        let outcomes: Vec<Outcome> = checks
            .iter()
            .map(|k| {
                let passed = script[&k.id].as_str().map(|n| text.contains(n)).unwrap_or(false);
                Outcome {
                    id: k.id.clone(),
                    required: k.required,
                    weight: k.weight.max(1),
                    passed,
                    took_ms: 0,
                    detail: String::new(),
                }
            })
            .collect();
        let accepted = outcomes.iter().filter(|o| o.required).all(|o| o.passed);
        let total: u32 = outcomes.iter().map(|o| o.weight).sum();
        let won: u32 = outcomes.iter().filter(|o| o.passed).map(|o| o.weight).sum();
        Verdict { accepted, score: if total == 0 { 0 } else { won * 1000 / total }, outcomes }
    }

    #[test]
    fn a_candidate_that_matches_every_rule_is_accepted() {
        let v = with_script(
            r#"{"a":"foo","b":"bar"}"#,
            candidate("foo and bar"),
            vec![check("a", true), check("b", true)],
        );
        assert!(v.accepted);
        assert_eq!(v.score, 1000);
    }

    #[test]
    fn a_missing_required_rule_closes_the_gate_but_the_score_still_orders() {
        let v = with_script(
            r#"{"a":"foo","b":"bar"}"#,
            candidate("foo only"),
            vec![check("a", true), check("b", true)],
        );
        assert!(!v.accepted, "b did not match");
        assert_eq!(v.score, 500, "but half the checks is half the score — the selection signal");
    }

    /// A check the script never mentions fails. A gate that passed the unscripted
    /// would prove the loop runs by proving nothing about the gate.
    #[test]
    fn an_unscripted_check_fails_rather_than_passes() {
        let v = with_script(
            r#"{"a":"foo"}"#,
            candidate("foo"),
            vec![check("a", true), check("z", true)],
        );
        assert!(!v.accepted, "z is not in the script");
        assert_eq!(v.score, 500);
    }
}
