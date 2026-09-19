//! `mock-jev-provider` — `jev:decision`, scripted.
//!
//! ## The script
//!
//! `mock-script` is JSON: an ordered list of rules, first match wins.
//!
//! ```json
//! {
//!   "rules": [
//!     { "when": "printer",     "selected": "helpdesk-domain", "confidence": 0.94 },
//!     { "when": "appointment", "selected": "clinic-domain",   "confidence": 0.9 },
//!     { "when": "vague",       "flat": true, "confidence": 0.2 },
//!     { "when": "explode",     "error": "provider-denied", "detail": "429" },
//!     { "when": "*",           "selected": "unknown", "confidence": 0.1, "flat": true }
//!   ]
//! }
//! ```
//!
//! `when` is a substring of `choice-request.state` (for `choose`) or
//! `score-request.state`/`gate-request.state` (for `score`/`gate`), or `"*"`
//! for anything. `selected` must be one of the request's own `options` unless
//! the rule sets `"flat": true`, in which case the first offered option is
//! used regardless (a flat/low-confidence answer is not claiming to know
//! which one is right). The remaining confidence is split evenly across the
//! other options for `distribution`, mirroring the real provider's shape
//! closely enough for a router's branching to be exercised end to end.
//!
//! A rule may return an error instead of a decision. `"error"` names any
//! `decision-error` case, the same convention `mock-provider` uses for
//! `infer-error`.
//!
//! `score` picks the rule's `"level"` (an index into `score-request.levels`,
//! default the middle level) as the reported `value`, and `gate` reads
//! `"probability"` (default 0.5) as the reported yes-probability. Both are
//! matched against the same rule list, by the same `when` substring rule.

#[allow(warnings)]
mod bindings;

use bindings::exports::jev::decision::decision::{
    AnswerKind, Answered, ChoiceRequest, ChoiceResult, DecisionError, GateRequest, GateResult,
    Guest, OptionScore, Question, QuestionKind, ScoreRequest, ScoreResult,
};
use bindings::wasi::config::store as config;

struct Component;

fn cfg(key: &str, default: &str) -> String {
    config::get(key).ok().flatten().filter(|s| !s.is_empty()).unwrap_or_else(|| default.to_string())
}

fn model_name() -> String {
    cfg("mock-model", "mock-jev-1")
}

fn error_for(name: &str, detail: &str) -> DecisionError {
    match name {
        "invalid-request" => DecisionError::InvalidRequest(detail.to_string()),
        "provider-denied" => DecisionError::ProviderDenied(detail.to_string()),
        "provider-unavailable" => DecisionError::ProviderUnavailable(detail.to_string()),
        "bad-response" => DecisionError::BadResponse(detail.to_string()),
        other => {
            DecisionError::InvalidRequest(format!("mock script names no such error: {other:?}"))
        }
    }
}

fn script() -> Result<serde_json::Value, DecisionError> {
    let raw = cfg("mock-script", "");
    if raw.is_empty() {
        return Err(DecisionError::InvalidRequest(
            "no mock-script configured — this provider is deliberately useless unscripted".into(),
        ));
    }
    serde_json::from_str(&raw)
        .map_err(|e| DecisionError::InvalidRequest(format!("mock-script is not JSON: {e}")))
}

/// Find the first rule whose `when` matches `text`, optionally scoped to one
/// question id (`evaluate`'s batch — everything else passes `None`, meaning a
/// rule's own `"question"` field, if it has one, is ignored). Order is
/// significant, so a specific rule can sit above a general one.
fn select_for<'a>(
    rules: &'a [serde_json::Value],
    text: &str,
    question_id: Option<&str>,
) -> Result<&'a serde_json::Value, DecisionError> {
    for rule in rules {
        let when = rule["when"].as_str().unwrap_or("*");
        if when != "*" && !text.contains(when) {
            continue;
        }
        if let Some(want) = rule["question"].as_str() {
            // A question-scoped rule only matches ITS OWN id — including
            // being skipped entirely when no id is given at all (the
            // single-question functions' own path).
            match question_id {
                Some(id) if id == want => {}
                _ => continue,
            }
        }
        return Ok(rule);
    }
    Err(DecisionError::InvalidRequest(format!(
        "no mock rule matches {text:?}; the script is out of date"
    )))
}

/// Find the first rule whose `when` matches `text`. Order is significant, so a
/// specific rule can sit above a general one.
fn select<'a>(
    rules: &'a [serde_json::Value],
    text: &str,
) -> Result<&'a serde_json::Value, DecisionError> {
    select_for(rules, text, None)
}

fn rules_of(s: &serde_json::Value) -> Vec<serde_json::Value> {
    s["rules"].as_array().cloned().unwrap_or_default()
}

/// Build a `choice-result` from a matched rule against the request's own
/// `options` — the mock can only ever answer with something offered.
fn choice_from(rule: &serde_json::Value, options: &[String]) -> Result<ChoiceResult, DecisionError> {
    if let Some(name) = rule["error"].as_str() {
        return Err(error_for(name, rule["detail"].as_str().unwrap_or_default()));
    }
    let confidence = ((rule["confidence"].as_f64().unwrap_or(0.5)).clamp(0.0, 1.0) * 1000.0) as u32;
    let flat = rule["flat"].as_bool().unwrap_or(false);
    let requested = rule["selected"].as_str();
    let selected = match requested.filter(|s| options.iter().any(|o| o == s)) {
        Some(s) => s.to_string(),
        None => options.first().cloned().ok_or_else(|| {
            DecisionError::InvalidRequest("no options offered".into())
        })?,
    };
    let remainder = if options.len() > 1 { (1000 - confidence) / (options.len() as u32 - 1) } else { 0 };
    let distribution = options
        .iter()
        .map(|o| OptionScore { label: o.clone(), score: if *o == selected { confidence } else { remainder } })
        .collect();
    Ok(ChoiceResult { selected, confidence, distribution, flat })
}

/// Build a `score-result` from a matched rule against the request's own
/// `levels`. `"level"` selects the reported (fractional) index, default the
/// midpoint — matching the reference provider's own default judgment.
fn score_from(rule: &serde_json::Value, levels: &[String]) -> Result<ScoreResult, DecisionError> {
    if let Some(name) = rule["error"].as_str() {
        return Err(error_for(name, rule["detail"].as_str().unwrap_or_default()));
    }
    let mid = (levels.len() - 1) as f64 / 2.0;
    let value = rule["level"].as_f64().unwrap_or(mid) as f32;
    let confidence = ((rule["confidence"].as_f64().unwrap_or(0.5)).clamp(0.0, 1.0) * 1000.0) as u32;
    let distribution = levels
        .iter()
        .map(|l| OptionScore { label: l.clone(), score: 1000 / levels.len() as u32 })
        .collect();
    Ok(ScoreResult { value, confidence, distribution })
}

/// Build a `gate-result` from a matched rule. `"probability"` is the reported
/// yes-probability, default 0.5 (dead uncertain, same default the reference
/// provider uses).
fn gate_from(rule: &serde_json::Value) -> Result<GateResult, DecisionError> {
    if let Some(name) = rule["error"].as_str() {
        return Err(error_for(name, rule["detail"].as_str().unwrap_or_default()));
    }
    let probability = ((rule["probability"].as_f64().unwrap_or(0.5)).clamp(0.0, 1.0) * 1000.0) as u32;
    Ok(GateResult { probability })
}

impl Guest for Component {
    fn choose(req: ChoiceRequest) -> Result<ChoiceResult, DecisionError> {
        if req.options.is_empty() {
            return Err(DecisionError::InvalidRequest("no options offered".into()));
        }
        let s = script()?;
        let rules = rules_of(&s);
        if rules.is_empty() {
            return Err(DecisionError::InvalidRequest(
                "mock-script has no rules — this provider answers nothing until it is scripted"
                    .into(),
            ));
        }
        let rule = select(&rules, &req.state)?;
        choice_from(rule, &req.options)
    }

    fn score(req: ScoreRequest) -> Result<ScoreResult, DecisionError> {
        if req.levels.len() < 2 {
            return Err(DecisionError::InvalidRequest("at least two levels are required".into()));
        }
        let rules = rules_of(&script()?);
        let rule = select(&rules, &req.state)?;
        score_from(rule, &req.levels)
    }

    fn gate(req: GateRequest) -> Result<GateResult, DecisionError> {
        if req.instructions.is_empty() {
            return Err(DecisionError::InvalidRequest("no instructions given".into()));
        }
        let rules = rules_of(&script()?);
        let rule = select(&rules, &req.state)?;
        gate_from(rule)
    }

    fn evaluate(state: String, questions: Vec<Question>) -> Result<Vec<Answered>, DecisionError> {
        if questions.is_empty() {
            return Err(DecisionError::InvalidRequest("no questions given".into()));
        }
        let rules = rules_of(&script()?);
        if rules.is_empty() {
            return Err(DecisionError::InvalidRequest(
                "mock-script has no rules — this provider answers nothing until it is scripted"
                    .into(),
            ));
        }
        Ok(questions
            .iter()
            .map(|q| {
                let outcome = select_for(&rules, &state, Some(&q.id)).and_then(|rule| match &q.kind {
                    QuestionKind::Choice(options) => choice_from(rule, options).map(AnswerKind::Choice),
                    QuestionKind::Score(levels) => score_from(rule, levels).map(AnswerKind::Score),
                    QuestionKind::Gate(_) => gate_from(rule).map(AnswerKind::Gate),
                });
                Answered { id: q.id.clone(), outcome }
            })
            .collect())
    }

    fn describe() -> (String, bool) {
        (model_name(), true)
    }
}

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::*;

    fn script_of(json: &str) -> Vec<serde_json::Value> {
        serde_json::from_str::<serde_json::Value>(json).unwrap()["rules"].as_array().unwrap().clone()
    }

    #[test]
    fn a_matching_rule_selects_the_scripted_option() {
        let rules = script_of(
            r#"{"rules":[{"when":"printer","selected":"helpdesk-domain","confidence":0.94}]}"#,
        );
        let options = vec!["helpdesk-domain".to_string(), "clinic-domain".to_string()];
        let rule = select(&rules, "the printer is jammed").unwrap();
        let out = choice_from(rule, &options).unwrap();
        assert_eq!(out.selected, "helpdesk-domain");
        assert_eq!(out.confidence, 940);
        assert!(!out.flat);
    }

    #[test]
    fn a_flat_rule_reports_low_confidence_and_no_confident_pick() {
        let rules = script_of(r#"{"rules":[{"when":"*","flat":true,"confidence":0.1}]}"#);
        let options = vec!["helpdesk-domain".to_string(), "clinic-domain".to_string()];
        let rule = select(&rules, "something ambiguous").unwrap();
        let out = choice_from(rule, &options).unwrap();
        assert!(out.flat);
        assert_eq!(out.confidence, 100);
    }

    #[test]
    fn the_first_matching_rule_wins() {
        let rules = script_of(
            r#"{"rules":[
                {"when":"specific","selected":"a","confidence":0.9},
                {"when":"*","selected":"b","confidence":0.5}
            ]}"#,
        );
        let options = vec!["a".to_string(), "b".to_string()];
        assert_eq!(choice_from(select(&rules, "a specific thing").unwrap(), &options).unwrap().selected, "a");
        assert_eq!(choice_from(select(&rules, "something else").unwrap(), &options).unwrap().selected, "b");
    }

    #[test]
    fn failure_modes_can_be_scripted() {
        let rules =
            script_of(r#"{"rules":[{"when":"explode","error":"provider-denied","detail":"429"}]}"#);
        let options = vec!["a".to_string()];
        match choice_from(select(&rules, "explode now").unwrap(), &options) {
            Err(DecisionError::ProviderDenied(m)) => assert!(m.contains("429")),
            other => panic!("expected a denial: {other:?}"),
        }
    }

    #[test]
    fn an_unmatched_state_is_an_error_not_silence() {
        let rules = script_of(r#"{"rules":[{"when":"only this","selected":"a"}]}"#);
        assert!(select(&rules, "something the script never anticipated").is_err());
    }

    #[test]
    fn a_selected_value_outside_the_offered_options_falls_back_to_the_first_option() {
        let rules = script_of(r#"{"rules":[{"when":"*","selected":"nonexistent","confidence":0.8}]}"#);
        let options = vec!["a".to_string(), "b".to_string()];
        let out = choice_from(select(&rules, "anything").unwrap(), &options).unwrap();
        assert_eq!(out.selected, "a");
    }

    #[test]
    fn score_reports_the_scripted_level() {
        let rules = script_of(r#"{"rules":[{"when":"*","level":2.0}]}"#);
        let levels = vec!["low".to_string(), "medium".to_string(), "high".to_string()];
        let out = score_from(select(&rules, "anything").unwrap(), &levels).unwrap();
        assert_eq!(out.value, 2.0);
    }

    #[test]
    fn gate_reports_the_scripted_probability() {
        let rules = script_of(r#"{"rules":[{"when":"urgent","probability":0.95}]}"#);
        let out = gate_from(select(&rules, "this is urgent").unwrap()).unwrap();
        assert_eq!(out.probability, 950);
    }

    #[test]
    fn a_question_scoped_rule_only_matches_its_own_id() {
        let rules = script_of(
            r#"{"rules":[
                {"question":"a","probability":0.9},
                {"question":"b","probability":0.1},
                {"when":"*","probability":0.5}
            ]}"#,
        );
        assert_eq!(select_for(&rules, "anything", Some("a")).unwrap()["probability"], 0.9);
        assert_eq!(select_for(&rules, "anything", Some("b")).unwrap()["probability"], 0.1);
        // No id given (the single-question functions' own path) ignores
        // question-scoped rules and falls through to the general one.
        assert_eq!(select_for(&rules, "anything", None).unwrap()["probability"], 0.5);
    }

    #[test]
    fn evaluate_answers_each_question_by_its_own_scripted_rule() {
        let rules = script_of(
            r#"{"rules":[
                {"question":"fits","probability":0.9},
                {"question":"doesnt-fit","probability":0.05}
            ]}"#,
        );
        for (id, want) in [("fits", 900), ("doesnt-fit", 50)] {
            let rule = select_for(&rules, "the goal text", Some(id)).unwrap();
            assert_eq!(gate_from(rule).unwrap().probability, want);
        }
    }
}
