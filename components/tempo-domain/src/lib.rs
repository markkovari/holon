//! tempo:app — a multi-person worktime logger over composed contracts.
//!
//! Accounts + RBAC come from the composed auth-guard (`auth:identity`). Two
//! global roles — admin (creates projects/categories, manages membership, sees
//! all) and member (default) — and per-project **memberships** (member | lead):
//! you log only against projects you belong to, and a project **lead** sees that
//! project's whole distribution (its managerial view). Projects, categories,
//! time entries, memberships, and the per-user running timer are `record:store`
//! collections. Owners (and admins) can edit/delete entries. A time entry carries a
//! `day` (YYYY-MM-DD, so a range filter is a string compare — the client owns
//! the calendar), denormalized project/category names for reporting, and
//! minutes. `GET /report` sums minutes grouped by project, category,
//! project×category, day, and user over a date range, scoped by the caller's
//! role — the shape the charts render. A "pomodoro" is just start (store now) →
//! stop (an entry with the elapsed minutes).

#[allow(warnings)]
mod bindings;

use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

use bindings::auth::identity::accounts;
use bindings::auth::identity::authorizer;
use bindings::auth::identity::rbac;
use bindings::auth::identity::session;
use bindings::auth::identity::types::{AuthError, Principal};
use bindings::pdf::codec::codec as pdf;
use bindings::records::store::store as records;
use bindings::wasi::clocks::wall_clock;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

struct Component;

const TENANT: &str = "tempo";
const PROJECTS: &str = "projects";
const CATEGORIES: &str = "categories";
const ENTRIES: &str = "entries";
const TIMERS: &str = "timers";
const USERS: &str = "users";
const MEMBERS: &str = "memberships"; // {project, user, email, role: member|lead}

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let method = request.method();
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        let outcome = match (&method, seg.as_slice()) {
            (Method::Get, [""]) => usage(),
            (Method::Post, ["api", "register"]) => register(&request),
            (Method::Post, ["api", "login"]) => login(&request),
            (Method::Post, ["api", "logout"]) => logout(&request),
            (Method::Get, ["api", "me"]) => me(&request),

            (Method::Post, ["api", "projects"]) => create_project(&request),
            (Method::Get, ["api", "projects"]) => list_projects(&request),
            (Method::Post, ["api", "projects", id, "members"]) => add_member(&request, id),
            (Method::Get, ["api", "projects", id, "members"]) => list_members(&request, id),
            (Method::Post, ["api", "categories"]) => create_category(&request),
            (Method::Get, ["api", "categories"]) => list_named(&request, CATEGORIES),

            (Method::Post, ["api", "entries"]) => create_entry(&request),
            (Method::Get, ["api", "entries"]) => list_entries(&request, &path),
            (Method::Patch, ["api", "entries", id]) => edit_entry(&request, id),
            (Method::Delete, ["api", "entries", id]) => delete_entry(&request, id),

            (Method::Post, ["api", "timer", "start"]) => timer_start(&request),
            (Method::Post, ["api", "timer", "stop"]) => timer_stop(&request),
            (Method::Get, ["api", "timer"]) => timer_get(&request),

            (Method::Get, ["api", "report"]) => report(&request, &path),
            (Method::Get, ["api", "report.pdf"]) => report_pdf(&request, &path),
            _ => Outcome::Err(404, "not_found".into()),
        };
        emit(response_out, outcome);
    }
}

enum Outcome {
    Json(u16, String),
    Err(u16, String),
    Auth(AuthError),
    // status, content-type, download filename, body.
    File(u16, String, String, Vec<u8>),
}

fn now() -> u64 {
    wall_clock::now().seconds
}

fn usage() -> Outcome {
    Outcome::Json(
        200,
        json!({
            "service": "tempo",
            "about": "multi-person worktime logger — log time by project + category (or run a pomodoro timer); role-scoped range reports drive the charts",
            "auth": "POST /api/register|login|logout, GET /api/me",
            "admin": "POST /api/projects {key,name}, POST /api/categories {name}",
            "log": "POST /api/entries {project, category, minutes, day, note?}",
            "timer": "POST /api/timer/start {project, category, day}, POST /api/timer/stop, GET /api/timer",
            "report": "GET /api/report?from=YYYY-MM-DD&to=YYYY-MM-DD&scope=me|all"
        })
        .to_string(),
    )
}

// ---- auth (auth-guard: auth:identity) ---------------------------------------

fn bearer(request: &IncomingRequest) -> Option<String> {
    let headers = request.headers();
    let vals = headers.get("authorization");
    let raw = vals.first()?;
    let s = String::from_utf8(raw.clone()).ok()?;
    s.strip_prefix("Bearer ").map(|t| t.to_string())
}

fn introspect(request: &IncomingRequest) -> Result<Principal, Outcome> {
    let token =
        bearer(request).ok_or(Outcome::Auth(AuthError::InvalidToken("missing bearer".into())))?;
    authorizer::introspect(&token).map_err(Outcome::Auth)
}

fn has_role(p: &Principal, role: &str) -> bool {
    p.roles.iter().any(|r| r == role)
}
fn is_admin(p: &Principal) -> bool {
    has_role(p, "admin")
}

// ---- project membership -----------------------------------------------------
// A regular user's reach is defined by per-project memberships (member | lead),
// not a global role. Admin transcends all of it.

/// This user's membership rows.
fn my_memberships(subject: &str) -> Vec<Value> {
    records::find_by(MEMBERS, "user", &json!(subject).to_string())
        .unwrap_or_default()
        .iter()
        .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok())
        .collect()
}

/// Project ids the user may log against: None = all (admin), else the set.
fn logging_projects(p: &Principal) -> Option<Vec<String>> {
    if is_admin(p) {
        return None;
    }
    Some(
        my_memberships(&p.subject)
            .iter()
            .filter_map(|m| m["project"].as_str().map(String::from))
            .collect(),
    )
}

/// Project ids the user leads (sees the whole distribution of): None = all (admin).
fn led_projects(p: &Principal) -> Option<Vec<String>> {
    if is_admin(p) {
        return None;
    }
    Some(
        my_memberships(&p.subject)
            .iter()
            .filter(|m| m["role"].as_str() == Some("lead"))
            .filter_map(|m| m["project"].as_str().map(String::from))
            .collect(),
    )
}

fn can_log(p: &Principal, project: &str) -> bool {
    match logging_projects(p) {
        None => true, // admin
        Some(ids) => ids.iter().any(|id| id == project),
    }
}

/// May the user see beyond their own time? admin, or a lead of ≥1 project.
fn can_see_all(p: &Principal) -> bool {
    match led_projects(p) {
        None => true,
        Some(ids) => !ids.is_empty(),
    }
}

fn is_lead_of(p: &Principal, project: &str) -> bool {
    match led_projects(p) {
        None => true,
        Some(ids) => ids.iter().any(|id| id == project),
    }
}

fn register(request: &IncomingRequest) -> Outcome {
    let body = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let email = body["email"].as_str().unwrap_or("").trim().to_string();
    let password = body["password"].as_str().unwrap_or("").to_string();
    let p = match accounts::register(&email, &password, TENANT) {
        Ok(p) => p,
        Err(e) => return Outcome::Auth(e),
    };
    // demo self-assign of the global role (an admin would grant this in prod).
    // managerial reach is per-project (lead), not a global role.
    let wanted = body["role"].as_str().unwrap_or("member");
    let role = if ["member", "admin"].contains(&wanted) { wanted } else { "member" };
    let _ = rbac::assign_role(&p.tenant, &p.subject, role);
    // remember the email for human-readable reports.
    let u = json!({ "subject": p.subject, "email": email });
    let _ = records::create(USERS, &u.to_string(), &["subject".to_string(), "email".to_string()]);
    Outcome::Json(201, json!({ "subject": p.subject, "roles": [role] }).to_string())
}

fn login(request: &IncomingRequest) -> Outcome {
    let body = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let email = body["email"].as_str().unwrap_or("").trim().to_string();
    let password = body["password"].as_str().unwrap_or("").to_string();
    match accounts::login(&email, &password, TENANT) {
        Ok(tp) => Outcome::Json(
            200,
            json!({ "access_token": tp.access_token, "refresh_token": tp.refresh_token, "expires_in": tp.expires_in, "session_id": tp.session_id }).to_string(),
        ),
        Err(e) => Outcome::Auth(e),
    }
}

fn me(request: &IncomingRequest) -> Outcome {
    match introspect(request) {
        Ok(p) => Outcome::Json(
            200,
            json!({ "subject": p.subject, "roles": p.roles, "email": email_of(&p.subject), "can_see_all": can_see_all(&p) }).to_string(),
        ),
        Err(o) => o,
    }
}

fn logout(request: &IncomingRequest) -> Outcome {
    let Some(token) = bearer(request) else {
        return Outcome::Auth(AuthError::InvalidToken("missing bearer".into()));
    };
    match session::revoke(&token) {
        Ok(()) => Outcome::Json(200, json!({ "ok": true }).to_string()),
        Err(e) => Outcome::Auth(e),
    }
}

fn email_of(subject: &str) -> String {
    records::find_by(USERS, "subject", &json!(subject).to_string())
        .ok()
        .and_then(|v| v.into_iter().next())
        .and_then(|e| serde_json::from_str::<Value>(&e.data).ok())
        .and_then(|d| d["email"].as_str().map(String::from))
        .unwrap_or_else(|| subject.to_string())
}

// ---- projects + categories (admin creates) ----------------------------------

fn create_project(request: &IncomingRequest) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    if !is_admin(&p) {
        return Outcome::Err(403, "admin only".into());
    }
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let key = b["key"].as_str().unwrap_or("").trim().to_string();
    let name = b["name"].as_str().unwrap_or("").trim().to_string();
    if key.is_empty() || name.is_empty() {
        return Outcome::Err(422, "key and name required".into());
    }
    let d = json!({ "id": Value::Null, "key": key, "name": name, "created": now() });
    save_named(PROJECTS, d)
}

fn create_category(request: &IncomingRequest) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    if !is_admin(&p) {
        return Outcome::Err(403, "admin only".into());
    }
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let name = b["name"].as_str().unwrap_or("").trim().to_string();
    if name.is_empty() {
        return Outcome::Err(422, "name required".into());
    }
    let d = json!({ "id": Value::Null, "name": name, "created": now() });
    save_named(CATEGORIES, d)
}

/// Create a record and write its own id back in (so views carry it).
fn save_named(collection: &str, mut d: Value) -> Outcome {
    let entry = match records::create(collection, &d.to_string(), &[]) {
        Ok(e) => e,
        Err(e) => return store_err(e),
    };
    d["id"] = json!(entry.id);
    let _ = records::update(collection, &entry.id, &d.to_string(), entry.revision);
    Outcome::Json(201, d.to_string())
}

fn list_named(request: &IncomingRequest, collection: &str) -> Outcome {
    if let Err(o) = introspect(request) {
        return o;
    }
    let items = load_all(collection);
    Outcome::Json(200, json!({ "items": items }).to_string())
}

/// Projects the caller belongs to (admin: all), each annotated with the caller's
/// role on it (`admin` | `lead` | `member`).
fn list_projects(request: &IncomingRequest) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let allow = logging_projects(&p); // None = admin (all)
    let leads = led_projects(&p);
    let items: Vec<Value> = load_all(PROJECTS)
        .into_iter()
        .filter(|d| {
            let id = d["id"].as_str().unwrap_or("");
            allow.as_ref().map(|ids| ids.iter().any(|x| x == id)).unwrap_or(true)
        })
        .map(|mut d| {
            let id = d["id"].as_str().unwrap_or("").to_string();
            let role = if is_admin(&p) {
                "admin"
            } else if leads.as_ref().map(|ids| ids.contains(&id)).unwrap_or(false) {
                "lead"
            } else {
                "member"
            };
            d["my_role"] = json!(role);
            d
        })
        .collect();
    Outcome::Json(200, json!({ "items": items }).to_string())
}

fn subject_for_email(email: &str) -> Option<String> {
    records::find_by(USERS, "email", &json!(email).to_string())
        .ok()?
        .into_iter()
        .next()
        .and_then(|e| serde_json::from_str::<Value>(&e.data).ok())
        .and_then(|d| d["subject"].as_str().map(String::from))
}

/// Admin or a project lead adds a user (by email) to the project as member|lead.
fn add_member(request: &IncomingRequest, project: &str) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    if !is_admin(&p) && !is_lead_of(&p, project) {
        return Outcome::Err(403, "admin or project lead only".into());
    }
    if !name_map(PROJECTS).contains_key(project) {
        return Outcome::Err(404, "no such project".into());
    }
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let email = b["email"].as_str().unwrap_or("").trim().to_string();
    let role = match b["role"].as_str().unwrap_or("member") {
        "lead" => "lead",
        _ => "member",
    };
    let subject = match subject_for_email(&email) {
        Some(s) => s,
        None => {
            return Outcome::Err(404, "no user with that email (they must register first)".into())
        }
    };
    // upsert: one membership row per (project, user).
    let existing = records::find_by(MEMBERS, "user", &json!(subject).to_string())
        .unwrap_or_default()
        .into_iter()
        .find(|e| {
            serde_json::from_str::<Value>(&e.data)
                .ok()
                .and_then(|d| d["project"].as_str().map(|x| x == project))
                .unwrap_or(false)
        });
    let d = json!({ "project": project, "user": subject, "email": email, "role": role, "created": now() });
    match existing {
        Some(e) => {
            let _ = records::update(MEMBERS, &e.id, &d.to_string(), 0);
        }
        None => {
            let _ = records::create(
                MEMBERS,
                &d.to_string(),
                &["user".to_string(), "project".to_string()],
            );
        }
    }
    Outcome::Json(200, d.to_string())
}

fn list_members(request: &IncomingRequest, project: &str) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    if !is_admin(&p) && !is_lead_of(&p, project) {
        return Outcome::Err(403, "admin or project lead only".into());
    }
    let members: Vec<Value> = records::find_by(MEMBERS, "project", &json!(project).to_string())
        .unwrap_or_default()
        .iter()
        .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok())
        .map(|d| json!({ "email": d["email"], "role": d["role"] }))
        .collect();
    Outcome::Json(200, json!({ "members": members }).to_string())
}

fn load_all(collection: &str) -> Vec<Value> {
    records::list_records(collection, 1000, "")
        .map(|p| p.entries)
        .unwrap_or_default()
        .iter()
        .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok())
        .collect()
}

/// id -> name map for a named collection.
fn name_map(collection: &str) -> BTreeMap<String, String> {
    load_all(collection)
        .into_iter()
        .filter_map(|d| {
            let id = d["id"].as_str()?.to_string();
            let name = d["name"].as_str().unwrap_or(&id).to_string();
            Some((id, name))
        })
        .collect()
}

// ---- entries (log time) -----------------------------------------------------

fn valid_day(d: &str) -> bool {
    let b = d.as_bytes();
    d.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter().enumerate().all(|(i, &c)| i == 4 || i == 7 || c.is_ascii_digit())
}

/// Build an entry record from (project, category, minutes, day, note) for
/// `subject`, resolving + denormalizing the project/category names. Returns the
/// record or an error Outcome.
fn build_entry(subject: &str, b: &Value) -> Result<Value, Outcome> {
    let project = b["project"].as_str().unwrap_or("").to_string();
    let category = b["category"].as_str().unwrap_or("").to_string();
    let minutes = b["minutes"].as_u64().unwrap_or(0);
    let day = b["day"].as_str().unwrap_or("").to_string();
    let note = b["note"].as_str().unwrap_or("").to_string();
    // optional time-of-day (minutes from midnight) for the calendar grid; -1 =
    // unscheduled.
    let start = b["start"].as_i64().unwrap_or(-1);
    if minutes == 0 {
        return Err(Outcome::Err(422, "minutes must be > 0".into()));
    }
    if !valid_day(&day) {
        return Err(Outcome::Err(422, "day must be YYYY-MM-DD".into()));
    }
    let projects = name_map(PROJECTS);
    let cats = name_map(CATEGORIES);
    let pname = match projects.get(&project) {
        Some(n) => n.clone(),
        None => return Err(Outcome::Err(422, "unknown project".into())),
    };
    let cname = match cats.get(&category) {
        Some(n) => n.clone(),
        None => return Err(Outcome::Err(422, "unknown category".into())),
    };
    Ok(json!({
        "id": Value::Null,
        "user": subject,
        "email": email_of(subject),
        "project": project, "project_name": pname,
        "category": category, "category_name": cname,
        "minutes": minutes, "day": day, "note": note, "start": start,
        "created": now(),
    }))
}

fn create_entry(request: &IncomingRequest) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    if !can_log(&p, b["project"].as_str().unwrap_or("")) {
        return Outcome::Err(403, "you are not a member of this project".into());
    }
    match build_entry(&p.subject, &b) {
        Ok(d) => save_indexed_entry(d),
        Err(o) => o,
    }
}

/// Edit own entry (owner or admin): minutes / category / day / note.
fn edit_entry(request: &IncomingRequest, id: &str) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let (mut d, rev) = match records::get(ENTRIES, id)
        .ok()
        .and_then(|e| serde_json::from_str::<Value>(&e.data).ok().map(|d| (d, e.revision)))
    {
        Some(x) => x,
        None => return Outcome::Err(404, "no such entry".into()),
    };
    if d["user"].as_str() != Some(&p.subject) && !is_admin(&p) {
        return Outcome::Err(403, "not your entry".into());
    }
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    if let Some(m) = b["minutes"].as_u64() {
        if m == 0 {
            return Outcome::Err(422, "minutes must be > 0".into());
        }
        d["minutes"] = json!(m);
    }
    if let Some(day) = b["day"].as_str() {
        if !valid_day(day) {
            return Outcome::Err(422, "day must be YYYY-MM-DD".into());
        }
        d["day"] = json!(day);
    }
    if let Some(cat) = b["category"].as_str() {
        match name_map(CATEGORIES).get(cat) {
            Some(n) => {
                d["category"] = json!(cat);
                d["category_name"] = json!(n);
            }
            None => return Outcome::Err(422, "unknown category".into()),
        }
    }
    if let Some(project) = b["project"].as_str() {
        match name_map(PROJECTS).get(project) {
            Some(n) => {
                // moving an entry to a project the caller can't log to is refused.
                if !can_log(&p, project) {
                    return Outcome::Err(403, "you are not a member of that project".into());
                }
                d["project"] = json!(project);
                d["project_name"] = json!(n);
            }
            None => return Outcome::Err(422, "unknown project".into()),
        }
    }
    if let Some(note) = b["note"].as_str() {
        d["note"] = json!(note);
    }
    if let Some(st) = b["start"].as_i64() {
        d["start"] = json!(st);
    }
    match records::update(ENTRIES, id, &d.to_string(), rev) {
        Ok(_) => Outcome::Json(200, d.to_string()),
        Err(e) => store_err(e),
    }
}

fn delete_entry(request: &IncomingRequest, id: &str) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let owner = records::get(ENTRIES, id)
        .ok()
        .and_then(|e| serde_json::from_str::<Value>(&e.data).ok())
        .and_then(|d| d["user"].as_str().map(String::from));
    match owner {
        Some(u) if u == p.subject || is_admin(&p) => {
            let _ = records::delete(ENTRIES, id);
            Outcome::Json(200, json!({ "ok": true }).to_string())
        }
        Some(_) => Outcome::Err(403, "not your entry".into()),
        None => Outcome::Err(404, "no such entry".into()),
    }
}

fn save_indexed_entry(mut d: Value) -> Outcome {
    let entry =
        match records::create(ENTRIES, &d.to_string(), &["user".to_string(), "day".to_string()]) {
            Ok(e) => e,
            Err(e) => return store_err(e),
        };
    d["id"] = json!(entry.id);
    let _ = records::update(ENTRIES, &entry.id, &d.to_string(), entry.revision);
    Outcome::Json(201, d.to_string())
}

/// Entries in [from,to] visible to the caller:
///   scope=me → own; scope=all → admin: everything, lead: their led projects'
///   entries (all users). A member asking for `all` falls back to own.
fn visible_entries(p: &Principal, from: &str, to: &str, scope_all: bool) -> Vec<Value> {
    let team = scope_all && can_see_all(p);
    let led = if team { led_projects(p) } else { Some(Vec::new()) }; // None = admin (all)
    load_all(ENTRIES)
        .into_iter()
        .filter(|e| {
            let day = e["day"].as_str().unwrap_or("");
            if day < from || day > to {
                return false;
            }
            if !team {
                return e["user"].as_str() == Some(&p.subject);
            }
            match &led {
                None => true, // admin
                Some(ids) => ids.iter().any(|id| Some(id.as_str()) == e["project"].as_str()),
            }
        })
        .collect()
}

fn list_entries(request: &IncomingRequest, path: &str) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let (from, to) = range(path);
    let scope_all = query_str(path, "scope").as_deref() == Some("all");
    let mut entries = visible_entries(&p, &from, &to, scope_all);
    entries.sort_by(|a, b| b["day"].as_str().cmp(&a["day"].as_str()));
    Outcome::Json(200, json!({ "entries": entries }).to_string())
}

// ---- pomodoro timer ---------------------------------------------------------

fn my_timer(subject: &str) -> Option<(String, Value)> {
    records::find_by(TIMERS, "user", &json!(subject).to_string())
        .ok()?
        .into_iter()
        .next()
        .and_then(|e| serde_json::from_str::<Value>(&e.data).ok().map(|d| (e.id, d)))
}

fn timer_start(request: &IncomingRequest) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let project = b["project"].as_str().unwrap_or("").to_string();
    let category = b["category"].as_str().unwrap_or("").to_string();
    let day = b["day"].as_str().unwrap_or("").to_string();
    if !valid_day(&day) {
        return Outcome::Err(422, "day must be YYYY-MM-DD".into());
    }
    if !name_map(PROJECTS).contains_key(&project) || !name_map(CATEGORIES).contains_key(&category) {
        return Outcome::Err(422, "unknown project or category".into());
    }
    let d = json!({
        "user": p.subject, "project": project, "category": category, "day": day,
        "note": b["note"].as_str().unwrap_or(""), "started": now(),
    });
    // one running timer per user — replace any existing.
    match my_timer(&p.subject) {
        Some((id, _)) => {
            let _ = records::update(TIMERS, &id, &d.to_string(), 0);
        }
        None => {
            let _ = records::create(TIMERS, &d.to_string(), &["user".to_string()]);
        }
    }
    Outcome::Json(200, d.to_string())
}

fn timer_stop(request: &IncomingRequest) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let Some((id, t)) = my_timer(&p.subject) else {
        return Outcome::Err(404, "no running timer".into());
    };
    let started = t["started"].as_u64().unwrap_or_else(now);
    let minutes = ((now().saturating_sub(started)) / 60).max(1); // at least a minute
    let entry = json!({
        "id": Value::Null, "user": p.subject, "email": email_of(&p.subject),
        "project": t["project"], "project_name": name_map(PROJECTS).get(t["project"].as_str().unwrap_or("")).cloned().unwrap_or_default(),
        "category": t["category"], "category_name": name_map(CATEGORIES).get(t["category"].as_str().unwrap_or("")).cloned().unwrap_or_default(),
        "minutes": minutes, "day": t["day"], "note": t["note"], "created": now(),
    });
    let _ = records::delete(TIMERS, &id);
    save_indexed_entry(entry)
}

fn timer_get(request: &IncomingRequest) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    match my_timer(&p.subject) {
        Some((_, t)) => Outcome::Json(200, json!({ "timer": t }).to_string()),
        None => Outcome::Json(200, json!({ "timer": Value::Null }).to_string()),
    }
}

// ---- report (the aggregation the charts render) -----------------------------

fn report(request: &IncomingRequest, path: &str) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    Outcome::Json(200, report_data(&p, path).to_string())
}

/// The aggregated range report as a JSON object — shared by the JSON endpoint
/// and the PDF export so both read exactly the same numbers.
fn report_data(p: &Principal, path: &str) -> Value {
    let (from, to) = range(path);
    let scope_all = query_str(path, "scope").as_deref() == Some("all");
    let entries = visible_entries(p, &from, &to, scope_all);

    let mut by_project: BTreeMap<String, u64> = BTreeMap::new();
    let mut pnames: BTreeMap<String, String> = BTreeMap::new();
    let mut by_category: BTreeMap<String, u64> = BTreeMap::new();
    let mut by_day: BTreeMap<String, u64> = BTreeMap::new();
    let mut by_user: BTreeMap<String, u64> = BTreeMap::new();
    let mut matrix: BTreeMap<(String, String), u64> = BTreeMap::new();
    let mut total = 0u64;

    for e in &entries {
        let m = e["minutes"].as_u64().unwrap_or(0);
        total += m;
        let proj = e["project"].as_str().unwrap_or("").to_string();
        let pname = e["project_name"].as_str().unwrap_or("").to_string();
        let cname = e["category_name"].as_str().unwrap_or("").to_string();
        pnames.insert(proj.clone(), pname.clone());
        *by_project.entry(proj.clone()).or_default() += m;
        *by_category.entry(cname.clone()).or_default() += m;
        *by_day.entry(e["day"].as_str().unwrap_or("").to_string()).or_default() += m;
        *by_user.entry(e["email"].as_str().unwrap_or("").to_string()).or_default() += m;
        *matrix.entry((pname, cname)).or_default() += m;
    }

    let arr_named = |m: &BTreeMap<String, u64>| -> Vec<Value> {
        let mut v: Vec<Value> = m.iter().map(|(k, n)| json!({ "key": k, "minutes": n })).collect();
        v.sort_by(|a, b| b["minutes"].as_u64().cmp(&a["minutes"].as_u64()));
        v
    };
    let by_project_v: Vec<Value> = {
        let mut v: Vec<Value> = by_project
            .iter()
            .map(|(id, n)| json!({ "project": id, "name": pnames.get(id).cloned().unwrap_or_default(), "minutes": n }))
            .collect();
        v.sort_by(|a, b| b["minutes"].as_u64().cmp(&a["minutes"].as_u64()));
        v
    };
    let matrix_v: Vec<Value> = matrix
        .iter()
        .map(|((p, c), n)| json!({ "project": p, "category": c, "minutes": n }))
        .collect();
    let by_day_v: Vec<Value> =
        by_day.iter().map(|(d, n)| json!({ "day": d, "minutes": n })).collect();

    let mut out = Map::new();
    out.insert("from".into(), json!(from));
    out.insert("to".into(), json!(to));
    out.insert("scope".into(), json!(if scope_all && can_see_all(p) { "all" } else { "me" }));
    out.insert("can_see_all".into(), json!(can_see_all(p)));
    out.insert("total_minutes".into(), json!(total));
    out.insert("by_project".into(), json!(by_project_v));
    out.insert("by_category".into(), json!(arr_named(&by_category)));
    out.insert("by_day".into(), json!(by_day_v));
    out.insert("matrix".into(), json!(matrix_v));
    if scope_all && can_see_all(p) {
        out.insert("by_user".into(), json!(arr_named(&by_user)));
    }
    Value::Object(out)
}

/// The same range report rendered to a downloadable PDF via `pdf:codec`.
fn report_pdf(request: &IncomingRequest, path: &str) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let r = report_data(&p, path);
    let hm = |min: u64| format!("{}h {:02}m", min / 60, min % 60);
    let line = |text: String, size: u32, bold: bool, gap: u32| pdf::Block {
        text,
        size,
        bold,
        gap_before: gap,
    };
    let mut blocks = vec![
        line(
            format!("{} — {}", r["from"].as_str().unwrap_or(""), r["to"].as_str().unwrap_or("")),
            11,
            false,
            0,
        ),
        line(format!("Scope: {}", r["scope"].as_str().unwrap_or("me")), 11, false, 0),
        line(format!("Total: {}", hm(r["total_minutes"].as_u64().unwrap_or(0))), 13, true, 4),
    ];
    let section = |blocks: &mut Vec<pdf::Block>, title: &str| {
        blocks.push(line(title.to_string(), 13, true, 14));
    };
    let rows = |v: &Value, key: &str| -> Vec<(String, u64)> {
        v.as_array()
            .map(|a| {
                a.iter()
                    .map(|x| {
                        (
                            x[key].as_str().unwrap_or("(none)").to_string(),
                            x["minutes"].as_u64().unwrap_or(0),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    section(&mut blocks, "By project");
    for (name, min) in rows(&r["by_project"], "name") {
        blocks.push(line(format!("{:<30} {}", trunc(&name, 30), hm(min)), 11, false, 0));
    }
    section(&mut blocks, "By category");
    for (name, min) in rows(&r["by_category"], "key") {
        blocks.push(line(format!("{:<30} {}", trunc(&name, 30), hm(min)), 11, false, 0));
    }
    if let Some(users) = r.get("by_user") {
        section(&mut blocks, "By person");
        for (name, min) in rows(users, "key") {
            blocks.push(line(format!("{:<30} {}", trunc(&name, 30), hm(min)), 11, false, 0));
        }
    }
    let doc = pdf::Document { title: "tempo — time report".to_string(), blocks };
    let bytes = pdf::render(&doc);
    let name = format!(
        "tempo-report-{}_{}.pdf",
        r["from"].as_str().unwrap_or(""),
        r["to"].as_str().unwrap_or("")
    );
    Outcome::File(200, "application/pdf".to_string(), name, bytes)
}

/// Truncate to `n` chars for fixed-width PDF columns (built-in fonts aren't
/// monospaced, but short labels keep the columns readable enough).
fn trunc(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n - 1).collect::<String>())
    }
}

// ---- http plumbing -----------------------------------------------------------

fn range(path: &str) -> (String, String) {
    let from = query_str(path, "from").unwrap_or_else(|| "0000-00-00".into());
    let to = query_str(path, "to").unwrap_or_else(|| "9999-99-99".into());
    (from, to)
}

fn query_str(path: &str, key: &str) -> Option<String> {
    let query = path.split('?').nth(1)?;
    query.split('&').find_map(|pair| {
        let mut it = pair.splitn(2, '=');
        if it.next()? == key {
            Some(it.next().unwrap_or("").replace("%3A", ":").replace("%2D", "-"))
        } else {
            None
        }
    })
}

fn store_err(e: records::StoreError) -> Outcome {
    match e {
        records::StoreError::NotFound => Outcome::Err(404, "not_found".into()),
        records::StoreError::InvalidJson(m) => Outcome::Err(422, m),
        records::StoreError::RevisionConflict(_) => Outcome::Err(409, "conflict".into()),
        records::StoreError::BackendUnavailable(m) => Outcome::Err(503, m),
    }
}

fn body(request: &IncomingRequest) -> Result<Value, Outcome> {
    let raw = read_body(request).map_err(|_| Outcome::Err(400, "could not read body".into()))?;
    if raw.is_empty() {
        return Ok(Value::Object(Default::default()));
    }
    serde_json::from_slice(&raw).map_err(|e| Outcome::Err(400, format!("bad json: {e}")))
}

/// The most a request body may be, before the component stops reading it.
///
/// There was no ceiling anywhere: 148 of 150 components accumulated whatever
/// arrived until the guest hit wasmtime's 64 MiB per-store memory cap and TRAPPED,
/// which reaches the caller as a closed connection saying nothing about a size.
/// A component that answers JSON has no business reading sixteen megabytes, and
/// the ones that legitimately handle uploads police it themselves with a 413 and a
/// granted max-size — those are left alone.
///
/// Generous on purpose. This is a backstop against an unbounded read, not a
/// content policy; an API that needs a real limit should state its own and say 413.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

guestio::guest_read_body!(MAX_BODY_BYTES);

fn emit(response_out: ResponseOutparam, result: Outcome) {
    if let Outcome::File(code, ctype, name, bytes) = result {
        let disp = format!("attachment; filename=\"{}\"", name);
        return respond(response_out, code, &ctype, Some(&disp), &bytes);
    }
    let (code, body) = match result {
        Outcome::Json(c, b) => (c, b),
        Outcome::Err(c, m) => (c, json!({ "error": m }).to_string()),
        Outcome::Auth(e) => {
            let msg = match &e {
                AuthError::InvalidToken(m) => m.clone(),
                AuthError::InvalidCredentials => "invalid credentials".into(),
                other => format!("{other:?}"),
            };
            (401, json!({ "error": msg }).to_string())
        }
        Outcome::File(..) => unreachable!(),
    };
    respond(response_out, code, "application/json", None, body.as_bytes());
}

fn respond(
    response_out: ResponseOutparam,
    status: u16,
    ctype: &str,
    disposition: Option<&str>,
    body: &[u8],
) {
    let headers = Fields::new();
    let _ = headers.set("content-type", &[ctype.as_bytes().to_vec()]);
    if let Some(d) = disposition {
        let _ = headers.set("content-disposition", &[d.as_bytes().to_vec()]);
    }
    let _ = headers.set("access-control-allow-origin", &[b"*".to_vec()]);
    let response = OutgoingResponse::new(headers);
    let _ = response.set_status_code(status);
    let out = response.body().expect("outgoing body");
    ResponseOutparam::set(response_out, Ok(response));
    if !body.is_empty() {
        let stream = out.write().expect("write stream");
        for chunk in body.chunks(4096) {
            let _ = stream.blocking_write_and_flush(chunk);
        }
    }
    let _ = OutgoingBody::finish(out, None);
}

bindings::export!(Component with_types_in bindings);
