//! Surveys (`open -> closed`) and the responses submitted against them. The
//! only authorization here is a plain role check (`is_admin`) plus one
//! business rule — a subject may answer a given survey at most once — which
//! is enforced against the `responses` collection directly, not with
//! `policy:guard`. There is no ownership concept, so no `owner-or-admin`
//! policy: just "admin for the management routes, anyone authenticated to
//! read and to answer an open survey".

use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use crate::{audit, introspect, is_admin, Reply, Route};
use serde_json::{json, Value};

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "surveys"]) => create_survey(route, body),
        (Method::Get, ["api", "surveys"]) => list_surveys(route),
        (Method::Post, ["api", "surveys", id, "responses"]) => create_response(route, id, body),
        (Method::Get, ["api", "surveys", id, "results"]) => results(route, id),
        (Method::Post, ["api", "surveys", id, "close"]) => close_survey(route, id),
        _ => Reply::err(404, "not_found"),
    }
}

fn parse(s: &str) -> Value {
    serde_json::from_str(s).unwrap_or(json!({}))
}

#[derive(serde::Deserialize)]
struct SurveyReq {
    #[serde(default)]
    title: String,
    #[serde(default)]
    questions: Vec<String>,
}

/// `admin` only: a non-admin is refused before any storage is touched. The
/// survey is created `open` — `close` is the only transition out of that.
fn create_survey(route: &Route, body: &str) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    guestauth::guest_deny_unless!(is_admin(&principal), principal, "survey.create", "");
    let req = guestauth::guest_parse_body!(body, SurveyReq);
    if req.title.is_empty() || req.questions.is_empty() {
        return Reply::err(400, "title and questions are required");
    }
    let data = json!({
        "title": req.title,
        "questions": req.questions,
        "status": "open",
    })
    .to_string();
    match records::create("surveys", &data, &[]) {
        Ok(entry) => {
            audit("survey.create", "allow", &principal.subject, &entry.id);
            Reply::json(201, json!({"id": entry.id}))
        }
        Err(_) => Reply::err(500, "store_error"),
    }
}

/// Every OPEN survey, for anyone authenticated; an admin also sees closed
/// ones. Status is filtered in Rust rather than by index so both callers can
/// share one list.
fn list_surveys(route: &Route) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    let admin = is_admin(&principal);
    match records::list_records("surveys", 100, "") {
        Ok(page) => {
            let surveys: Vec<Value> = page
                .entries
                .iter()
                .filter_map(|e| {
                    let mut v = parse(&e.data);
                    if !admin && v.get("status").and_then(Value::as_str) != Some("open") {
                        return None;
                    }
                    if let Value::Object(ref mut m) = v {
                        m.insert("id".to_string(), json!(e.id));
                    }
                    Some(v)
                })
                .collect();
            Reply::json(200, json!({"surveys": surveys}))
        }
        Err(_) => Reply::err(500, "store_error"),
    }
}

#[derive(serde::Deserialize)]
struct ResponseReq {
    #[serde(default)]
    answers: Vec<String>,
}

/// Any authenticated subject may answer, but only an existing, still-`open`
/// survey (400 otherwise), only with exactly as many answers as the survey
/// has questions (400 otherwise), and at most once per survey (409) — that
/// last one is a uniqueness check against the `responses` collection, not a
/// policy call.
fn create_response(route: &Route, id: &str, body: &str) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    let req = guestauth::guest_parse_body!(body, ResponseReq);
    let survey = guestauth::guest_get_or_404!("surveys", id);
    let survey_data = parse(&survey.data);
    if survey_data.get("status").and_then(Value::as_str) != Some("open") {
        return Reply::err(400, "survey is not open");
    }
    let question_count =
        survey_data.get("questions").and_then(Value::as_array).map(Vec::len).unwrap_or(0);
    if req.answers.len() != question_count {
        return Reply::err(400, "wrong number of answers");
    }

    // Both `survey_id` and `respondent` are indexed on create, so one query
    // answers "has this subject already replied to this survey?".
    let survey_id_json = serde_json::to_string(id).unwrap_or_default();
    let respondent_json = serde_json::to_string(&principal.subject).unwrap_or_default();
    let filters = vec![
        records::Filter { field: "survey_id".to_string(), value: survey_id_json },
        records::Filter { field: "respondent".to_string(), value: respondent_json },
    ];
    if let Ok(existing) = records::query("responses", &filters, 1) {
        if !existing.is_empty() {
            audit("response.create", "deny", &principal.subject, id);
            return Reply::err(409, "already_responded");
        }
    }

    let data = json!({
        "survey_id": id,
        "respondent": principal.subject,
        "answers": req.answers,
    })
    .to_string();
    let indexes = vec!["survey_id".to_string(), "respondent".to_string()];
    match records::create("responses", &data, &indexes) {
        Ok(entry) => {
            audit("response.create", "allow", &principal.subject, id);
            Reply::json(201, json!({"id": entry.id}))
        }
        Err(_) => Reply::err(500, "store_error"),
    }
}

/// `admin` only: every response to a survey, with respondent and answers.
fn results(route: &Route, id: &str) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    guestauth::guest_deny_unless!(is_admin(&principal), principal, "survey.results", id);
    if records::get("surveys", id).is_err() {
        return Reply::err(404, "not_found");
    }
    let survey_id_json = serde_json::to_string(id).unwrap_or_default();
    match records::find_by("responses", "survey_id", &survey_id_json) {
        Ok(entries) => {
            let responses: Vec<Value> = entries
                .iter()
                .map(|e| {
                    let v = parse(&e.data);
                    json!({
                        "respondent": v.get("respondent").cloned().unwrap_or(json!("")),
                        "answers": v.get("answers").cloned().unwrap_or(json!([])),
                    })
                })
                .collect();
            Reply::json(
                200,
                json!({
                    "survey_id": id,
                    "response_count": responses.len(),
                    "responses": responses,
                }),
            )
        }
        Err(_) => Reply::err(500, "store_error"),
    }
}

/// `admin` only: `open -> closed`, and refused (400) if it is already closed.
fn close_survey(route: &Route, id: &str) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    guestauth::guest_deny_unless!(is_admin(&principal), principal, "survey.close", id);
    let entry = guestauth::guest_get_or_404!("surveys", id);
    let mut survey = parse(&entry.data);
    if survey.get("status").and_then(Value::as_str) != Some("open") {
        return Reply::err(400, "survey is already closed");
    }
    survey["status"] = json!("closed");
    match records::update("surveys", id, &survey.to_string(), entry.revision) {
        Ok(_) => {
            audit("survey.close", "allow", &principal.subject, id);
            Reply::json(200, json!({"id": id, "status": "closed"}))
        }
        Err(_) => Reply::err(409, "conflict"),
    }
}