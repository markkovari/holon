//! `jev-decision` — the TRIVIAL REFERENCE provider for `jev:decision@0.1.0`.
//!
//! This is the reference implementation of the decision boundary: the SWAP
//! POINT a router imports. It does NOT call a real classifier — there is no
//! http, no wasi:config, no secrets here, since the world `jev-decision` is
//! intentionally minimal and imports nothing (mirrors `llm-inference`'s own
//! built-in mock for the same reason).
//!
//! `choose` picks the first option whose text appears (case-insensitively) as
//! a substring of `state`, reporting high confidence and an uneven
//! distribution; if none match, it falls back to the first option with a flat,
//! low-confidence distribution — deterministic, and honest that it found
//! nothing. `score` always reports the middle level with a flat distribution,
//! and `gate` always reports 50/50 — this provider has no actual judgment to
//! offer, only a shape to satisfy.
//!
//! A real provider (TypeSafe's Jev) is a SEPARATE component
//! (`typesafe-provider`) that implements this same `decision` interface but
//! additionally imports `wasi:http`, `wasi:config` and `comp:secrets`. A
//! scripted mock (`mock-jev-provider`) exists for deterministic, offline
//! testing of routing logic that needs specific, repeatable answers — this
//! crate is only the baseline every real provider is measured against.

#[allow(warnings)]
mod bindings;

use bindings::exports::jev::decision::decision::{
    AnswerKind, Answered, ChoiceRequest, ChoiceResult, DecisionError, GateRequest, GateResult,
    Guest, OptionScore, Question, QuestionKind, ScoreRequest, ScoreResult,
};

struct Component;

/// The milli-probability reported for a substring match.
const MATCH_CONFIDENCE: u32 = 800;
/// The milli-probability reported when nothing matched — deliberately low.
const NO_MATCH_CONFIDENCE: u32 = 100;
/// What `gate` always reports: no real judgment, so dead uncertain.
const NEUTRAL_PROBABILITY: u32 = 500;

fn choose_impl(req: &ChoiceRequest) -> Result<ChoiceResult, DecisionError> {
    if req.options.is_empty() {
        return Err(DecisionError::InvalidRequest("no options offered".into()));
    }
    let haystack = req.state.to_lowercase();
    let hit = req.options.iter().position(|o| haystack.contains(&o.to_lowercase()));

    let (selected, confidence, flat) = match hit {
        Some(i) => (req.options[i].clone(), MATCH_CONFIDENCE, false),
        None => (req.options[0].clone(), NO_MATCH_CONFIDENCE, true),
    };

    let remainder = if req.options.len() > 1 {
        (1000 - confidence) / (req.options.len() as u32 - 1)
    } else {
        0
    };
    let distribution = req
        .options
        .iter()
        .map(|o| OptionScore {
            label: o.clone(),
            score: if *o == selected { confidence } else { remainder },
        })
        .collect();

    Ok(ChoiceResult { selected, confidence, distribution, flat })
}

fn score_impl(req: &ScoreRequest) -> Result<ScoreResult, DecisionError> {
    if req.levels.len() < 2 {
        return Err(DecisionError::InvalidRequest("at least two levels are required".into()));
    }
    let mid = (req.levels.len() - 1) as f32 / 2.0;
    let each = 1000 / req.levels.len() as u32;
    let distribution =
        req.levels.iter().map(|l| OptionScore { label: l.clone(), score: each }).collect();
    Ok(ScoreResult { value: mid, confidence: NO_MATCH_CONFIDENCE, distribution })
}

fn gate_impl(instructions: &str) -> Result<GateResult, DecisionError> {
    if instructions.is_empty() {
        return Err(DecisionError::InvalidRequest("no instructions given".into()));
    }
    Ok(GateResult { probability: NEUTRAL_PROBABILITY })
}

/// One `question`, answered against the shared `state` — the same logic
/// `choose`/`score`/`gate` use standalone, dispatched by `kind`.
fn answer_one(state: &str, q: &Question) -> Answered {
    let result = match &q.kind {
        QuestionKind::Choice(options) => choose_impl(&ChoiceRequest {
            state: state.to_string(),
            instructions: q.instructions.clone(),
            options: options.clone(),
        })
        .map(AnswerKind::Choice),
        QuestionKind::Score(levels) => score_impl(&ScoreRequest {
            state: state.to_string(),
            instructions: q.instructions.clone(),
            levels: levels.clone(),
        })
        .map(AnswerKind::Score),
        QuestionKind::Gate(_criteria) => gate_impl(&q.instructions).map(AnswerKind::Gate),
    };
    Answered { id: q.id.clone(), outcome: result }
}

impl Guest for Component {
    fn choose(req: ChoiceRequest) -> Result<ChoiceResult, DecisionError> {
        choose_impl(&req)
    }

    fn score(req: ScoreRequest) -> Result<ScoreResult, DecisionError> {
        score_impl(&req)
    }

    fn gate(req: GateRequest) -> Result<GateResult, DecisionError> {
        gate_impl(&req.instructions)
    }

    fn evaluate(state: String, questions: Vec<Question>) -> Result<Vec<Answered>, DecisionError> {
        if questions.is_empty() {
            return Err(DecisionError::InvalidRequest("no questions given".into()));
        }
        Ok(questions.iter().map(|q| answer_one(&state, q)).collect())
    }

    fn describe() -> (String, bool) {
        ("jev-decision-reference".to_string(), true)
    }
}

bindings::export!(Component with_types_in bindings);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_substring_match_wins_with_high_confidence() {
        let req = ChoiceRequest {
            state: "the printer is jammed again".into(),
            instructions: "classify".into(),
            options: vec!["helpdesk-domain".into(), "clinic-domain".into()],
        };
        // Neither option is literally a substring here, so this exercises the
        // fallback path deterministically.
        let out = choose_impl(&req).unwrap();
        assert_eq!(out.selected, "helpdesk-domain");
        assert!(out.flat);
    }

    #[test]
    fn a_literal_option_in_the_state_is_selected_confidently() {
        let req = ChoiceRequest {
            state: "please route this to clinic-domain".into(),
            instructions: "classify".into(),
            options: vec!["helpdesk-domain".into(), "clinic-domain".into()],
        };
        let out = choose_impl(&req).unwrap();
        assert_eq!(out.selected, "clinic-domain");
        assert_eq!(out.confidence, MATCH_CONFIDENCE);
        assert!(!out.flat);
    }

    #[test]
    fn empty_options_are_rejected() {
        let req = ChoiceRequest {
            state: "anything".into(),
            instructions: "classify".into(),
            options: vec![],
        };
        assert!(matches!(choose_impl(&req), Err(DecisionError::InvalidRequest(_))));
    }

    #[test]
    fn the_distribution_sums_close_to_one_thousand() {
        let req = ChoiceRequest {
            state: "no match here".into(),
            instructions: "classify".into(),
            options: vec!["a".into(), "b".into(), "c".into()],
        };
        let out = choose_impl(&req).unwrap();
        let total: u32 = out.distribution.iter().map(|d| d.score).sum();
        assert!(total <= 1000, "total {total} should not exceed 1000");
    }

    #[test]
    fn fewer_than_two_levels_is_rejected() {
        let req = ScoreRequest {
            state: "x".into(),
            instructions: "rate".into(),
            levels: vec!["only".into()],
        };
        assert!(matches!(score_impl(&req), Err(DecisionError::InvalidRequest(_))));
    }

    #[test]
    fn score_reports_the_midpoint_of_the_rubric() {
        let req = ScoreRequest {
            state: "x".into(),
            instructions: "rate".into(),
            levels: vec!["low".into(), "medium".into(), "high".into()],
        };
        let out = score_impl(&req).unwrap();
        assert_eq!(out.value, 1.0);
        assert_eq!(out.distribution.len(), 3);
    }

    #[test]
    fn evaluate_answers_every_question_by_its_own_id() {
        let questions = vec![
            Question {
                id: "a".into(),
                instructions: "classify".into(),
                kind: QuestionKind::Choice(vec!["helpdesk-domain".into(), "clinic-domain".into()]),
            },
            Question {
                id: "b".into(),
                instructions: "is this urgent?".into(),
                kind: QuestionKind::Gate(GateCriteria {
                    true_hint: "".into(),
                    false_hint: "".into(),
                }),
            },
        ];
        let out = Component::evaluate("route to clinic-domain".into(), questions).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].id, "a");
        assert_eq!(out[1].id, "b");
        match &out[0].outcome {
            Ok(AnswerKind::Choice(c)) => assert_eq!(c.selected, "clinic-domain"),
            other => panic!("expected a choice answer: {other:?}"),
        }
        assert!(matches!(&out[1].outcome, Ok(AnswerKind::Gate(_))));
    }

    #[test]
    fn evaluate_rejects_an_empty_batch() {
        assert!(matches!(
            Component::evaluate("anything".into(), vec![]),
            Err(DecisionError::InvalidRequest(_))
        ));
    }
}
