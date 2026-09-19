//! Surveys and responses. `admin` creates surveys, closes them and reads
//! results; anyone authenticated fills in an open survey exactly once.
//!
//! Two rules need enforcing that `auth:identity/rbac` cannot express, and
//! neither of them is ownership:
//!
//!   - "a subject may answer a given survey at most once" — a uniqueness
//!     check across the `responses` collection, done with `records::find_by`
//!     on `survey_id` and filtered in Rust on `respondent`;
//!   - "answers must line up with the survey's questions" — a shape check on
//!     the submitted array.
//!
//! Both are plain handler logic. There is no `policy:guard` here and no
//! `guest_owner_or_admin_policy!`: no row belongs to anybody, so an ABAC
//! rule would be the wrong tool.

use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use crate::{audit, introspect, is_admin, Reply, Route};
use serde_json::{json, Value};

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "surveys"]) => create_survey(route, body),
        (Method::Get, ["api", "surveys"]) => list_surveys(route),
        (Method::Post, ["api", "surveys", id, "responses"]) => submit_response(route, id, body),
        (Method::Get, ["api", "surveys", id, "results"]) => results(route, id),
        (Method::Post, ["api", "surveys", id, "close"]) => close_survey(route, id),
        _ => Reply::err(404, "not_found"),
    }
}

/// The JSON-encoded form of a string, which is what `find_by`/`query` index
/// and match on — a bare value would never match an indexed string field.
fn enc(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())
}

fn data_of(entry: &records::Entry) -> Value {
    serde_json::from_str(&entry.data).unwrap_or_else(|_| json!({}))
}

#[derive(serde::Deserialize)]
struct SurveyReq {
    #[serde(default)]
    title: String,
    #[serde(default)]
    questions: Vec<String>,
}

/// `admin`-only. A survey is born `open`; `status` is indexed so non-admins
/// can list exactly the open ones without reading every survey.
fn create_survey(route: &Route, body: &str) -> Reply {
    let principal = match introspect(route) {
        Ok(p) => p,
        Err(r) => return r,
    };
    if !is_admin(&principal) {
        audit("survey.create", "deny", &principal.subject, "");
        return Reply::err(403, "forbidden");
    }
    let req: SurveyReq = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return Reply::err(400, "bad_json"),
    };
    if req.title.is_empty() || req.questions.is_empty() {
        return Reply::err(400, "title and at least one question are required");
    }
    let data =
        json!({"title": req.title, "questions": req.questions, "status": "open"}).to_string();
    match records::create("surveys", &data, &["status".to_string()]) {
        Ok(entry) => {
            audit("survey.create", "allow", &principal.subject, &entry.id);
            Reply::json(201, json!({"id": entry.id}))
        }
        Err(_) => Reply::err(500, "store_error"),
    }
}

/// Any authenticated caller. `admin` sees every survey, everyone else only
/// the `open` ones — a role decides the visibility, not a per-row attribute.
fn list_surveys(route: &Route) -> Reply {
    let principal = match introspect(route) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let result = if is_admin(&principal) {
        records::list_records("surveys", 100, "").map(|p| p.entries)
    } else {
        records::find_by("surveys", "status", &enc("open"))
    };
    match result {
        Ok(entries) => Reply::json(200, json!({"surveys": entries_json(&entries)})),
        Err(_) => Reply::err(500, "store_error"),
    }
}

#[derive(serde::Deserialize)]
struct ResponseReq {
    #[serde(default)]
    answers: Vec<String>,
}

/// Any authenticated caller. The order of refusals matters and is the order
/// the rules are written in: a closed survey is 400 whatever else is wrong
/// with the submission, a wrong number of answers is 400 before uniqueness
/// is even considered, and only a well-formed second attempt is 409.
fn submit_response(route: &Route, id: &str, body: &str) -> Reply {
    let principal = match introspect(route) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let req: ResponseReq = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return Reply::err(400, "bad_json"),
    };
    let entry = match records::get("surveys", id) {
        Ok(e) => e,
        Err(_) => return Reply::err(404, "not_found"),
    };
    let survey = data_of(&entry);
    if survey.get("status").and_then(Value::as_str) != Some("open") {
        return Reply::err(400, "survey is not open");
    }
    let question_count = survey
        .get("questions")
        .and_then(Value::as_array)
        .map(|a| a.len())
        .unwrap_or(0);
    if req.answers.len() != question_count {
        return Reply::err(400, "answers must match the number of questions");
    }
    if already_responded(id, &principal.subject) {
        audit("response.create", "deny", &principal.subject, id);
        return Reply::err(409, "already_responded");
    }
    let data = json!({
        "survey_id": id,
        "respondent": principal.subject,
        "answers": req.answers,
    })
    .to_string();
    match records::create("responses", &data, &["survey_id".to_string()]) {
        Ok(entry) => {
            audit("response.create", "allow", &principal.subject, &entry.id);
            Reply::json(201, json!({"id": entry.id}))
        }
        Err(_) => Reply::err(500, "store_error"),
    }
}

/// One response per (survey, respondent). `survey_id` is indexed, so the
/// store does the narrowing; `respondent` is compared in Rust rather than
/// paying for a second index on a field that is only ever read together with
/// the first.
fn already_responded(survey_id: &str, subject: &str) -> bool {
    let entries = records::find_by("responses", "survey_id", &enc(survey_id)).unwrap_or_default();
    entries.iter().any(|e| {
        data_of(e).get("respondent").and_then(Value::as_str) == Some(subject)
    })
}

/// `admin`-only. Not a page of raw records: the caller gets the shape they
/// asked for — a count and one entry per respondent.
fn results(route: &Route, id: &str) -> Reply {
    let principal = match introspect(route) {
        Ok(p) => p,
        Err(r) => return r,
    };
    if !is_admin(&principal) {
        audit("survey.results", "deny", &principal.subject, id);
        return Reply::err(403, "forbidden");
    }
    let entries = records::find_by("responses", "survey_id", &enc(id)).unwrap_or_default();
    let responses: Vec<Value> = entries
        .iter()
        .map(|e| {
            let v = data_of(e);
            json!({
                "respondent": v.get("respondent").cloned().unwrap_or(json!("")),
                "answers": v.get("answers").cloned().unwrap_or(json!([])),
            })
        })
        .collect();
    let count = responses.len();
    Reply::json(
        200,
        json!({"survey_id": id, "response_count": count, "responses": responses}),
    )
}

/// `admin`-only, `open -> closed`, once. Closing an already-closed survey is
/// a 400 (the request is malformed for the current state), not a 409 — there
/// is no revision conflict to report, the record simply is not in the state
/// the caller assumed.
fn close_survey(route: &Route, id: &str) -> Reply {
    let principal = match introspect(route) {
        Ok(p) => p,
        Err(r) => return r,
    };
    if !is_admin(&principal) {
        audit("survey.close", "deny", &principal.subject, id);
        return Reply::err(403, "forbidden");
    }
    let entry = match records::get("surveys", id) {
        Ok(e) => e,
        Err(_) => return Reply::err(404, "not_found"),
    };
    let mut survey = data_of(&entry);
    if survey.get("status").and_then(Value::as_str) == Some("closed") {
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

/// The record's own JSON with its store id folded in — the `id` a client
/// needs to act on a survey that `find_by`/`query` only return as a record.
fn entries_json(entries: &[records::Entry]) -> Vec<Value> {
    entries
        .iter()
        .map(|e| {
            let mut v = data_of(e);
            if let Value::Object(ref mut m) = v {
                m.insert("id".to_string(), json!(e.id));
            }
            v
        })
        .collect()
}