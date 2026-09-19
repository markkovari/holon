//! Job postings (admin-created) and candidates through a hiring pipeline
//! (`applied -> screening -> interview -> offer -> hired`/`rejected`). An
//! `interviewer` only acts on postings assigned to them — the row-level rule
//! `auth:identity/rbac` cannot express, enforced with `policy:guard` exactly
//! as `crm-domain` enforces "a rep only acts on their own deals". Creating a
//! posting is `admin`-only, checked directly against `principal.roles`.

use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use crate::{audit, introspect, is_admin, Reply, Route};
use serde_json::{json, Value};

const STAGES: &[&str] = &["applied", "screening", "interview", "offer", "hired", "rejected"];
const POLICY_DOMAIN: &str = "postings";

guestauth::guest_owner_or_admin_policy!(POLICY_DOMAIN, "assigned_to");
guestauth::guest_entries_json!();

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "postings"]) => create_posting(route, body),
        (Method::Get, ["api", "postings"]) => list_postings(route),
        (Method::Post, ["api", "postings", id, "candidates"]) => add_candidate(route, id, body),
        (Method::Get, ["api", "postings", id, "candidates"]) => list_candidates(route, id),
        (Method::Post, ["api", "candidates", id, "stage"]) => move_stage(route, id, body),
        _ => Reply::err(404, "not_found"),
    }
}

#[derive(serde::Deserialize)]
struct PostingReq {
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
    /// The interviewer's subject id (from their `/register` or `/me` response).
    #[serde(default)]
    assigned_to: String,
}

fn create_posting(route: &Route, body: &str) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    guestauth::guest_deny_unless!(is_admin(&principal), principal, "posting.create", "");
    let req = guestauth::guest_parse_body!(body, PostingReq);
    if req.title.is_empty() {
        return Reply::err(400, "title is required");
    }
    let data = json!({
        "title": req.title,
        "description": req.description,
        "assigned_to": req.assigned_to,
    })
    .to_string();
    match records::create("postings", &data, &["assigned_to".to_string()]) {
        Ok(entry) => {
            audit("posting.create", "allow", &principal.subject, &entry.id);
            Reply::json(201, json!({"id": entry.id}))
        }
        Err(_) => Reply::err(500, "store_error"),
    }
}

fn list_postings(route: &Route) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    let result = if is_admin(&principal) {
        records::list_records("postings", 100, "").map(|p| p.entries)
    } else {
        let subject_json = serde_json::to_string(&principal.subject).unwrap_or_default();
        records::find_by("postings", "assigned_to", &subject_json)
    };
    match result {
        Ok(entries) => Reply::json(200, json!({"postings": entries_json(&entries)})),
        Err(_) => Reply::err(500, "store_error"),
    }
}

fn posting_assigned_to(id: &str) -> Option<String> {
    let entry = records::get("postings", id).ok()?;
    let v: Value = serde_json::from_str(&entry.data).ok()?;
    v.get("assigned_to").and_then(Value::as_str).map(str::to_string)
}

#[derive(serde::Deserialize)]
struct CandidateReq {
    #[serde(default)]
    name: String,
    #[serde(default)]
    email: String,
}

fn add_candidate(route: &Route, posting_id: &str, body: &str) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    let Some(assigned_to) = posting_assigned_to(posting_id) else {
        return Reply::err(404, "not_found");
    };
    guestauth::guest_deny_unless!(owns_or_admin("edit", &principal, &assigned_to), principal, "candidate.create", posting_id);
    let req = guestauth::guest_parse_body!(body, CandidateReq);
    if req.name.is_empty() {
        return Reply::err(400, "name is required");
    }
    let data = json!({
        "posting_id": posting_id,
        "name": req.name,
        "email": req.email,
        "stage": "applied",
    })
    .to_string();
    match records::create("candidates", &data, &["posting_id".to_string()]) {
        Ok(entry) => {
            audit("candidate.create", "allow", &principal.subject, &entry.id);
            Reply::json(201, json!({"id": entry.id}))
        }
        Err(_) => Reply::err(500, "store_error"),
    }
}

fn list_candidates(route: &Route, posting_id: &str) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    let Some(assigned_to) = posting_assigned_to(posting_id) else {
        return Reply::err(404, "not_found");
    };
    guestauth::guest_deny_unless!(owns_or_admin("view", &principal, &assigned_to), principal, "candidate.list", posting_id);
    let posting_json = serde_json::to_string(&posting_id).unwrap_or_default();
    match records::find_by("candidates", "posting_id", &posting_json) {
        Ok(entries) => Reply::json(200, json!({"candidates": entries_json(&entries)})),
        Err(_) => Reply::err(500, "store_error"),
    }
}

fn move_stage(route: &Route, id: &str, body: &str) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    let entry = match records::get("candidates", id) {
        Ok(e) => e,
        Err(_) => return Reply::err(404, "not_found"),
    };
    let mut candidate: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));
    let posting_id = candidate.get("posting_id").and_then(Value::as_str).unwrap_or("").to_string();
    let Some(assigned_to) = posting_assigned_to(&posting_id) else {
        return Reply::err(404, "not_found");
    };
    guestauth::guest_deny_unless!(owns_or_admin("edit", &principal, &assigned_to), principal, "candidate.stage", id);
    let req: Value = serde_json::from_str(body).unwrap_or(json!({}));
    let stage = req.get("stage").and_then(Value::as_str).unwrap_or("");
    if !STAGES.contains(&stage) {
        return Reply::err(400, "invalid stage");
    }
    candidate["stage"] = json!(stage);
    match records::update("candidates", id, &candidate.to_string(), entry.revision) {
        Ok(_) => {
            audit("candidate.stage", "allow", &principal.subject, &format!("{id} -> {stage}"));
            Reply::json(200, json!({"id": id, "stage": stage}))
        }
        Err(_) => Reply::err(409, "conflict"),
    }
}
