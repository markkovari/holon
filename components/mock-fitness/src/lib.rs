//! Scores from a `gate-script` config (check id -> substring the changed files must contain), with no egress.

#[allow(warnings)]
mod bindings;

use bindings::exports::graph::fitness::evaluator::{
    Candidate, Check, CheckState, EvalError, Guest, Outcome, Verdict,
};
use bindings::wasi::config::store as config;

struct Component;

fn script() -> serde_json::Value {
    let raw = config::get("gate-script").ok().flatten().unwrap_or_default();
    serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null)
}

/// The base tree is deliberately excluded: matching it would pass a candidate for what it inherited.
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

        let mut outcomes: Vec<Outcome> = checks
            .iter()
            .map(|k| {
                // A check with no rule in the script fails, not passes: an unscripted gate waving everything through proves nothing.
                let passed =
                    script[&k.id].as_str().map(|needle| text.contains(needle)).unwrap_or(false);
                Outcome {
                    id: k.id.clone(),
                    required: k.required,
                    weight: k.weight.max(1),
                    state: if passed { CheckState::Passed } else { CheckState::Failed },
                    blocked_by: String::new(),
                    took_ms: 0,
                    detail: if passed { String::new() } else { format!("missing: {}", k.id) },
                }
            })
            .collect();

        // Same arithmetic as the real evaluator, graph included: ignoring it would pass a test against a gate the real one blocks.
        let mut stopped: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::new();
        for o in outcomes.iter() {
            if o.state != CheckState::Passed {
                stopped.insert(o.id.clone(), o.id.clone());
            }
        }
        for (i, k) in checks.iter().enumerate() {
            if let Some(root) = k.needs.iter().find_map(|n| stopped.get(n).cloned()) {
                outcomes[i].state = CheckState::NotAttempted;
                outcomes[i].blocked_by = root.clone();
                outcomes[i].detail = String::new();
                stopped.insert(k.id.clone(), root);
            }
        }

        let accepted =
            outcomes.iter().filter(|o| o.required).all(|o| o.state == CheckState::Passed);
        let total: u32 = outcomes.iter().map(|o| o.weight).sum();
        let won: u32 =
            outcomes.iter().filter(|o| o.state == CheckState::Passed).map(|o| o.weight).sum();
        let score = (won * 1000).checked_div(total).unwrap_or(0);

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
        Check { id: id.into(), required, weight: 1, command: vec![], needs: vec![] }
    }

    fn with_script(script: &str, c: Candidate, checks: Vec<Check>) -> Verdict {
        // `evaluate` reads config, which a unit test cannot set, so exercise the arithmetic directly against a parsed script.
        let script: serde_json::Value = serde_json::from_str(script).unwrap();
        let text = changed_text(&c);
        let mut outcomes: Vec<Outcome> = checks
            .iter()
            .map(|k| {
                let passed = script[&k.id].as_str().map(|n| text.contains(n)).unwrap_or(false);
                Outcome {
                    id: k.id.clone(),
                    required: k.required,
                    weight: k.weight.max(1),
                    state: if passed { CheckState::Passed } else { CheckState::Failed },
                    blocked_by: String::new(),
                    took_ms: 0,
                    detail: String::new(),
                }
            })
            .collect();
        let mut stopped: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::new();
        for o in outcomes.iter() {
            if o.state != CheckState::Passed {
                stopped.insert(o.id.clone(), o.id.clone());
            }
        }
        for (i, k) in checks.iter().enumerate() {
            if let Some(root) = k.needs.iter().find_map(|n| stopped.get(n).cloned()) {
                outcomes[i].state = CheckState::NotAttempted;
                outcomes[i].blocked_by = root.clone();
                stopped.insert(k.id.clone(), root);
            }
        }
        let accepted =
            outcomes.iter().filter(|o| o.required).all(|o| o.state == CheckState::Passed);
        let total: u32 = outcomes.iter().map(|o| o.weight).sum();
        let won: u32 =
            outcomes.iter().filter(|o| o.state == CheckState::Passed).map(|o| o.weight).sum();
        Verdict { accepted, score: (won * 1000).checked_div(total).unwrap_or(0), outcomes }
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
