//! Pure TypeSafe/Jev request-building + response-parsing, decoupled from the
//! WIT bindings so it is unit-testable on the host (`cargo test`). The Guest
//! impl in `lib.rs` converts the WIT records to/from these plain types at the
//! edges.
//!
//! Everything here is deterministic and dependency-light (serde_json only) —
//! no WASI, no config, no HTTP. The HTTP plumbing lives in `lib.rs`.
//!
//! ## Wire shape (from docs.typesafe.ai/api, docs.typesafe.ai/primitives/*)
//!
//! One endpoint, `POST /v1/systemone`, takes a `state` (the content being
//! evaluated) and a `questions` map (id -> Question), and returns an
//! `answers` map keyed the same way. `choose`/`score`/`gate` each send exactly
//! one question, under the fixed id [`QUESTION_ID`]; `evaluate` sends the
//! caller's own ids, all in one request — the batching Jev's API already
//! supports natively, and System One's own docs recommend over serial calls.
//!
//! ```json
//! // -> {"state": "...", "model": "jev-latest",
//! //     "questions": {"q": {"type": "choice", "instructions": "...",
//! //                          "criteria": {"a": null, "b": null}}}}
//! // <- {"model": "jev-latest",
//! //     "answers": {"q": {"type": "choice", "choice": "a",
//! //                       "probabilities": {"a": 0.87, "b": 0.13},
//! //                       "confidence": 0.87}},
//! //     "usage": {"input_tokens": 12, "output_tokens": 4}}
//! ```
//!
//! Score's `criteria` is the ordered `levels` array; its answer's
//! `probabilities` (and `legend`) are keyed by the level's 0-based position,
//! stringified (`"0"`, `"1"`, ...) — this module reconstructs the by-name
//! distribution `jev:decision` exposes from that index.
//!
//! Neither `probabilities` on a `score` answer is guaranteed present (the
//! docs' own quickstart example omits it), so it is treated as optional,
//! defaulting to no signal beyond `score`/`confidence`.
//!
//! Jev never reports a "flat distribution" flag itself — the docs describe it
//! only qualitatively ("a flat shape ... means low confidence"). `flat` in
//! `jev:decision::choice-result` is computed here: true when the top two
//! candidates in the distribution are within [`FLAT_MARGIN_MILLI`] of each
//! other, i.e. the provider could not clearly separate them.

use serde::Deserialize;
use serde_json::{json, Map, Value};

/// The fixed question id `choose`/`score`/`gate` use — `evaluate` uses the
/// caller's own ids instead.
const QUESTION_ID: &str = "q";

/// Milli-probability margin below which the top two candidates in a
/// `choose` distribution are considered indistinguishable.
const FLAT_MARGIN_MILLI: u32 = 100;

/// A parsed choice result (plain mirror of the WIT `choice-result`).
pub struct ParsedChoice {
    pub selected: String,
    pub confidence: u32,
    pub distribution: Vec<(String, u32)>,
    pub flat: bool,
}

/// A parsed score result (plain mirror of the WIT `score-result`).
pub struct ParsedScore {
    pub value: f32,
    pub confidence: u32,
    pub distribution: Vec<(String, u32)>,
}

/// Why parsing a response failed (mapped to `decision-error` by the caller).
#[derive(Debug)]
pub enum ParseError {
    BadResponse(String),
}

/// A 0.0..=1.0 float from the wire -> milli-units (0..=1000), clamped.
fn frac_to_milli(f: f64) -> u32 {
    (f.clamp(0.0, 1.0) * 1000.0).round() as u32
}

// ---- one question, as JSON (shared by the single-question and batch bodies) ---

fn choice_question(instructions: &str, options: &[String]) -> Value {
    let criteria: Map<String, Value> = options.iter().map(|o| (o.clone(), Value::Null)).collect();
    json!({ "type": "choice", "instructions": instructions, "criteria": criteria })
}

fn score_question(instructions: &str, levels: &[String]) -> Value {
    json!({ "type": "score", "instructions": instructions, "criteria": levels })
}

/// `true_hint`/`false_hint` are omitted individually when empty, and
/// `criteria` is omitted entirely when both are — Jev's own hints are
/// optional.
fn gate_question(instructions: &str, true_hint: &str, false_hint: &str) -> Value {
    let mut question = Map::new();
    question.insert("type".to_string(), Value::String("noul".to_string()));
    question.insert("instructions".to_string(), Value::String(instructions.to_string()));
    let mut criteria = Map::new();
    if !true_hint.is_empty() {
        criteria.insert("true".to_string(), Value::String(true_hint.to_string()));
    }
    if !false_hint.is_empty() {
        criteria.insert("false".to_string(), Value::String(false_hint.to_string()));
    }
    if !criteria.is_empty() {
        question.insert("criteria".to_string(), Value::Object(criteria));
    }
    Value::Object(question)
}

fn systemone_body(model: &str, state: &str, questions: Map<String, Value>) -> String {
    json!({ "state": state, "model": model, "questions": questions }).to_string()
}

/// Build the `/v1/systemone` request body for a Choice question.
/// `options` become criteria keys with no description (`null`) — this
/// contract carries labels only, never per-option guidance text.
pub fn choose_body(model: &str, state: &str, instructions: &str, options: &[String]) -> String {
    let mut questions = Map::new();
    questions.insert(QUESTION_ID.to_string(), choice_question(instructions, options));
    systemone_body(model, state, questions)
}

/// Build the `/v1/systemone` request body for a Score question. `levels` is
/// sent verbatim as the ordered `criteria` array (at least two entries).
pub fn score_body(model: &str, state: &str, instructions: &str, levels: &[String]) -> String {
    let mut questions = Map::new();
    questions.insert(QUESTION_ID.to_string(), score_question(instructions, levels));
    systemone_body(model, state, questions)
}

/// Build the `/v1/systemone` request body for a Noul (gate) question.
pub fn gate_body(
    model: &str,
    state: &str,
    instructions: &str,
    true_hint: &str,
    false_hint: &str,
) -> String {
    let mut questions = Map::new();
    questions.insert(QUESTION_ID.to_string(), gate_question(instructions, true_hint, false_hint));
    systemone_body(model, state, questions)
}

/// One question inside a batch — a plain mirror of the WIT `question`/
/// `question-kind`, so this module stays free of WASI bindings.
pub struct QuestionItem {
    pub id: String,
    pub instructions: String,
    pub kind: QuestionSpec,
}

pub enum QuestionSpec {
    Choice(Vec<String>),
    Score(Vec<String>),
    Gate { true_hint: String, false_hint: String },
}

/// One answer inside a batch — a plain mirror of the WIT `answer-kind`.
pub enum AnswerSpec {
    Choice(ParsedChoice),
    Score(ParsedScore),
    /// Milli-probability.
    Gate(u32),
}

/// Build the `/v1/systemone` request body for a whole batch — every
/// `questions` entry keyed by the caller's own id, in ONE request.
pub fn evaluate_body(model: &str, state: &str, questions: &[QuestionItem]) -> String {
    let mut map = Map::new();
    for q in questions {
        let qjson = match &q.kind {
            QuestionSpec::Choice(options) => choice_question(&q.instructions, options),
            QuestionSpec::Score(levels) => score_question(&q.instructions, levels),
            QuestionSpec::Gate { true_hint, false_hint } => {
                gate_question(&q.instructions, true_hint, false_hint)
            }
        };
        map.insert(q.id.clone(), qjson);
    }
    systemone_body(model, state, map)
}

#[derive(Deserialize)]
struct SystemOneResp {
    #[serde(default)]
    answers: Map<String, Value>,
}

/// Parse the response envelope once; callers look their own id(s) up in what
/// this returns.
fn answers_of(body: &[u8]) -> Result<Map<String, Value>, ParseError> {
    let parsed: SystemOneResp = serde_json::from_slice(body)
        .map_err(|e| ParseError::BadResponse(format!("systemone json: {e}")))?;
    Ok(parsed.answers)
}

fn find_answer(answers: &Map<String, Value>, id: &str) -> Result<Value, ParseError> {
    answers
        .get(id)
        .cloned()
        .ok_or_else(|| ParseError::BadResponse(format!("no answer for question {id:?}")))
}

/// A Choice answer. `options` is the request's own option list — a `choice`
/// value outside it is a bad response, not a value to trust.
fn choice_from_value(answer: &Value, options: &[String]) -> Result<ParsedChoice, ParseError> {
    let selected = answer["choice"]
        .as_str()
        .ok_or_else(|| ParseError::BadResponse("choice answer has no `choice`".into()))?
        .to_string();
    if !options.iter().any(|o| o == &selected) {
        return Err(ParseError::BadResponse(format!(
            "provider chose {selected:?}, which was not offered"
        )));
    }
    let confidence = frac_to_milli(answer["confidence"].as_f64().unwrap_or(0.0));
    let probabilities = answer["probabilities"].as_object();
    let distribution: Vec<(String, u32)> = options
        .iter()
        .map(|o| {
            let p = probabilities.and_then(|m| m.get(o)).and_then(Value::as_f64).unwrap_or(0.0);
            (o.clone(), frac_to_milli(p))
        })
        .collect();
    let flat = {
        let mut scores: Vec<u32> = distribution.iter().map(|(_, s)| *s).collect();
        scores.sort_unstable_by(|a, b| b.cmp(a));
        scores.len() >= 2 && scores[0] - scores[1] < FLAT_MARGIN_MILLI
    };
    Ok(ParsedChoice { selected, confidence, distribution, flat })
}

/// A Score answer. `levels` is the request's own rubric — the distribution is
/// reconstructed from the wire's index-keyed `probabilities` (`"0"`, `"1"`,
/// ...) back into `levels` order.
fn score_from_value(answer: &Value, levels: &[String]) -> Result<ParsedScore, ParseError> {
    let value = answer["score"]
        .as_f64()
        .ok_or_else(|| ParseError::BadResponse("score answer has no `score`".into()))?
        as f32;
    let confidence = frac_to_milli(answer["confidence"].as_f64().unwrap_or(0.0));
    let probabilities = answer["probabilities"].as_object();
    let distribution: Vec<(String, u32)> = levels
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let p = probabilities
                .and_then(|m| m.get(&i.to_string()))
                .and_then(Value::as_f64)
                .unwrap_or(0.0);
            (l.clone(), frac_to_milli(p))
        })
        .collect();
    Ok(ParsedScore { value, confidence, distribution })
}

/// A Noul answer, as a milli-probability.
fn gate_from_value(answer: &Value) -> Result<u32, ParseError> {
    let noul = answer["noul"]
        .as_f64()
        .ok_or_else(|| ParseError::BadResponse("noul answer has no `noul`".into()))?;
    Ok(frac_to_milli(noul))
}

/// Parse a Choice response (the `choose_body` request's answer).
pub fn parse_choice(body: &[u8], options: &[String]) -> Result<ParsedChoice, ParseError> {
    let answers = answers_of(body)?;
    choice_from_value(&find_answer(&answers, QUESTION_ID)?, options)
}

/// Parse a Score response (the `score_body` request's answer).
pub fn parse_score(body: &[u8], levels: &[String]) -> Result<ParsedScore, ParseError> {
    let answers = answers_of(body)?;
    score_from_value(&find_answer(&answers, QUESTION_ID)?, levels)
}

/// Parse a Noul response (the `gate_body` request's answer).
pub fn parse_gate(body: &[u8]) -> Result<u32, ParseError> {
    let answers = answers_of(body)?;
    gate_from_value(&find_answer(&answers, QUESTION_ID)?)
}

/// Parse a whole batch response — one entry per `questions`, in the same
/// order, each independently `Ok`/`Err` (a provider that left one id out of
/// its `answers` map fails only that entry, not the whole batch). Only the
/// envelope itself (bad JSON) fails the whole call.
pub fn parse_evaluate(
    body: &[u8],
    questions: &[QuestionItem],
) -> Result<Vec<(String, Result<AnswerSpec, ParseError>)>, ParseError> {
    let answers = answers_of(body)?;
    Ok(questions
        .iter()
        .map(|q| {
            let per_item = find_answer(&answers, &q.id).and_then(|answer| match &q.kind {
                QuestionSpec::Choice(options) => {
                    choice_from_value(&answer, options).map(AnswerSpec::Choice)
                }
                QuestionSpec::Score(levels) => {
                    score_from_value(&answer, levels).map(AnswerSpec::Score)
                }
                QuestionSpec::Gate { .. } => gate_from_value(&answer).map(AnswerSpec::Gate),
            });
            (q.id.clone(), per_item)
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choose_body_shapes_a_valid_request_with_null_criteria() {
        let options = vec!["billing".to_string(), "technical".to_string()];
        let v: serde_json::Value =
            serde_json::from_str(&choose_body("jev-latest", "help me", "route this", &options)).unwrap();
        assert_eq!(v["state"], "help me");
        assert_eq!(v["model"], "jev-latest");
        assert_eq!(v["questions"]["q"]["type"], "choice");
        assert_eq!(v["questions"]["q"]["instructions"], "route this");
        assert!(v["questions"]["q"]["criteria"]["billing"].is_null());
        assert!(v["questions"]["q"]["criteria"]["technical"].is_null());
    }

    #[test]
    fn score_body_sends_levels_as_the_criteria_array() {
        let levels = vec!["low".to_string(), "medium".to_string(), "high".to_string()];
        let v: serde_json::Value =
            serde_json::from_str(&score_body("jev-latest", "a report", "rate severity", &levels)).unwrap();
        assert_eq!(v["questions"]["q"]["type"], "score");
        assert_eq!(v["questions"]["q"]["criteria"], serde_json::json!(["low", "medium", "high"]));
    }

    #[test]
    fn gate_body_omits_criteria_when_no_hints_are_given() {
        let v: serde_json::Value =
            serde_json::from_str(&gate_body("jev-latest", "hi", "is this urgent?", "", "")).unwrap();
        assert_eq!(v["questions"]["q"]["type"], "noul");
        assert!(v["questions"]["q"].get("criteria").is_none());
    }

    #[test]
    fn gate_body_includes_only_the_hints_given() {
        let v: serde_json::Value = serde_json::from_str(&gate_body(
            "jev-latest",
            "hi",
            "is this urgent?",
            "mentions ASAP",
            "",
        ))
        .unwrap();
        assert_eq!(v["questions"]["q"]["criteria"]["true"], "mentions ASAP");
        assert!(v["questions"]["q"]["criteria"].get("false").is_none());
    }

    #[test]
    fn evaluate_body_sends_every_question_under_its_own_id_in_one_request() {
        let questions = vec![
            QuestionItem {
                id: "fit-a".into(),
                instructions: "does capability a fit?".into(),
                kind: QuestionSpec::Gate { true_hint: "".into(), false_hint: "".into() },
            },
            QuestionItem {
                id: "fit-b".into(),
                instructions: "does capability b fit?".into(),
                kind: QuestionSpec::Gate { true_hint: "".into(), false_hint: "".into() },
            },
        ];
        let v: serde_json::Value =
            serde_json::from_str(&evaluate_body("jev-latest", "a goal", &questions)).unwrap();
        assert_eq!(v["questions"].as_object().unwrap().len(), 2);
        assert_eq!(v["questions"]["fit-a"]["instructions"], "does capability a fit?");
        assert_eq!(v["questions"]["fit-b"]["instructions"], "does capability b fit?");
    }

    #[test]
    fn parse_choice_reads_selection_confidence_and_distribution() {
        let body = br#"{"model":"jev-latest","answers":{"q":{"type":"choice","choice":"billing",
            "probabilities":{"billing":0.84,"technical":0.159,"sales":0.001},"confidence":0.596}}}"#;
        let options = vec!["billing".to_string(), "technical".to_string(), "sales".to_string()];
        let p = parse_choice(body, &options).ok().unwrap();
        assert_eq!(p.selected, "billing");
        assert_eq!(p.confidence, 596);
        assert_eq!(
            p.distribution,
            vec![("billing".to_string(), 840), ("technical".to_string(), 159), ("sales".to_string(), 1)]
        );
        assert!(!p.flat, "billing (840) clearly leads technical (159)");
    }

    #[test]
    fn parse_choice_detects_a_flat_distribution() {
        let body = br#"{"answers":{"q":{"type":"choice","choice":"a",
            "probabilities":{"a":0.51,"b":0.49},"confidence":0.51}}}"#;
        let options = vec!["a".to_string(), "b".to_string()];
        let p = parse_choice(body, &options).ok().unwrap();
        assert!(p.flat, "51/49 is not a clear lead");
    }

    #[test]
    fn parse_choice_rejects_a_selection_outside_the_offered_options() {
        let body = br#"{"answers":{"q":{"type":"choice","choice":"c","confidence":0.9}}}"#;
        let options = vec!["a".to_string(), "b".to_string()];
        assert!(matches!(parse_choice(body, &options), Err(ParseError::BadResponse(_))));
    }

    #[test]
    fn parse_choice_bad_json_is_bad_response() {
        let options = vec!["a".to_string()];
        assert!(matches!(parse_choice(b"not json", &options), Err(ParseError::BadResponse(_))));
    }

    #[test]
    fn parse_score_reconstructs_the_distribution_from_index_keyed_probabilities() {
        let body = br#"{"answers":{"q":{"type":"score","score":1.3,"confidence":0.54,
            "legend":{"0":"cosmetic","1":"degraded","2":"blocking"},
            "probabilities":{"0":0.0,"1":0.7,"2":0.3}}}}"#;
        let levels = vec!["cosmetic".to_string(), "degraded".to_string(), "blocking".to_string()];
        let p = parse_score(body, &levels).ok().unwrap();
        assert_eq!(p.value, 1.3);
        assert_eq!(p.confidence, 540);
        assert_eq!(
            p.distribution,
            vec![
                ("cosmetic".to_string(), 0),
                ("degraded".to_string(), 700),
                ("blocking".to_string(), 300)
            ]
        );
    }

    #[test]
    fn parse_score_tolerates_a_missing_probabilities_field() {
        let body = br#"{"answers":{"q":{"type":"score","score":1.035,"confidence":0.842,
            "legend":{"0":"calm","1":"frustrated","2":"angry"}}}}"#;
        let levels = vec!["calm".to_string(), "frustrated".to_string(), "angry".to_string()];
        let p = parse_score(body, &levels).ok().unwrap();
        assert_eq!(p.value, 1.035);
        assert!(p.distribution.iter().all(|(_, s)| *s == 0));
    }

    #[test]
    fn parse_gate_reads_the_noul_probability() {
        let body = br#"{"answers":{"q":{"type":"noul","noul":0.93}}}"#;
        assert_eq!(parse_gate(body).ok().unwrap(), 930);
    }

    #[test]
    fn parse_gate_bad_json_is_bad_response() {
        assert!(matches!(parse_gate(b"not json"), Err(ParseError::BadResponse(_))));
    }

    #[test]
    fn parse_evaluate_matches_each_answer_to_its_own_question_by_id() {
        let body = br#"{"answers":{
            "fit-a":{"type":"noul","noul":0.9},
            "fit-b":{"type":"noul","noul":0.1}
        }}"#;
        let questions = vec![
            QuestionItem {
                id: "fit-a".into(),
                instructions: "x".into(),
                kind: QuestionSpec::Gate { true_hint: "".into(), false_hint: "".into() },
            },
            QuestionItem {
                id: "fit-b".into(),
                instructions: "x".into(),
                kind: QuestionSpec::Gate { true_hint: "".into(), false_hint: "".into() },
            },
        ];
        let out = parse_evaluate(body, &questions).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].0, "fit-a");
        match &out[0].1 {
            Ok(AnswerSpec::Gate(p)) => assert_eq!(*p, 900),
            other => panic!("expected a gate answer: {}", other.is_ok()),
        }
        match &out[1].1 {
            Ok(AnswerSpec::Gate(p)) => assert_eq!(*p, 100),
            other => panic!("expected a gate answer: {}", other.is_ok()),
        }
    }

    #[test]
    fn parse_evaluate_fails_only_the_missing_questions_own_entry() {
        let body = br#"{"answers":{"present":{"type":"noul","noul":0.5}}}"#;
        let questions = vec![
            QuestionItem {
                id: "present".into(),
                instructions: "x".into(),
                kind: QuestionSpec::Gate { true_hint: "".into(), false_hint: "".into() },
            },
            QuestionItem {
                id: "missing".into(),
                instructions: "x".into(),
                kind: QuestionSpec::Gate { true_hint: "".into(), false_hint: "".into() },
            },
        ];
        let out = parse_evaluate(body, &questions).unwrap();
        assert!(out[0].1.is_ok());
        assert!(out[1].1.is_err());
    }

    #[test]
    fn milli_conversion_clamps_at_the_boundaries() {
        assert_eq!(frac_to_milli(0.0), 0);
        assert_eq!(frac_to_milli(1.0), 1000);
        assert_eq!(frac_to_milli(1.5), 1000, "clamped above 1.0");
        assert_eq!(frac_to_milli(-0.5), 0, "clamped below 0.0");
    }
}
