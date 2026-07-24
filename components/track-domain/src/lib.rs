//! track:app — a Linear-lite project tracker over composed contracts.
//!
//! Five axes in one component (see wit/track.wit):
//!   write  — records + ids + md
//!   auth   — auth-guard introspect + role gating; project membership via policy:guard
//!   read   — search:index + paginate
//!   stream — every mutation publishes to event:bus; /api/stream is an SSE feed
//!   bg     — /api/tick sweeps stale in_progress issues
//!   out    — an issue move fires a webhook:sign-signed notify:dispatch webhook
//!   AI     — /api/issues/{id}/summarize condenses the comment thread
//!
//! Auth model: an account has a global role (admin can create projects); write
//! access to a project's issues is a per-project ABAC membership decision made
//! by policy:guard (a member/lead of the project may write). The issue
//! lifecycle is an fsm:workflow machine, mirrored onto the issue record.

#[allow(warnings)]
mod bindings;

use serde::Deserialize;
use serde_json::{json, Value};

use bindings::ai::inference::inference as ai;
use bindings::auth::identity::accounts;
use bindings::auth::identity::authorizer;
use bindings::auth::identity::rbac;
use bindings::auth::identity::session;
use bindings::auth::identity::types::{AuthError, Principal};
use bindings::event::bus::bus;
use bindings::fsm::workflow::engine as fsm;
use bindings::md::render::renderer as md;
use bindings::notify::dispatch::dispatcher as notify;
use bindings::paginate::cursor::cursors as paginate;
use bindings::policy::guard::guard as policy;
use bindings::records::store::store as records;
use bindings::search::index::index as search;
use bindings::ui::assets::files as statics;
use bindings::webhook::sign::signer as sign;
use bindings::wasi::clocks::monotonic_clock;
use bindings::wasi::clocks::wall_clock;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

struct Component;

const TENANT: &str = "track";
const PROJECTS: &str = "projects";
const ISSUES: &str = "issues";
const COMMENTS: &str = "comments";
const MEMBERS: &str = "members";
const MACHINE: &str = "issue";
const ACTIVITY: &str = "activity"; // event-bus topic driving the SSE feed
const POLICY_DOMAIN: &str = "project-write";
const WEBHOOK_SECRET: &str = "track-outbound-webhook-secret";
const STALE_SECS: u64 = 7 * 24 * 3600; // in_progress older than a week is flagged
const POLL_MS: u64 = 1000;
const MAX_TICKS: u32 = 3600;

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let method = request.method();
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        // SSE is written directly to the response stream — handle before `emit`.
        if let (Method::Get, ["api", "stream"]) = (&method, seg.as_slice()) {
            return stream_events(response_out, &path);
        }

        let result = match (&method, seg.as_slice()) {
            (Method::Get, ["api", "usage"]) => usage_json(),
            (Method::Post, ["auth", "register"]) => register(&request),
            (Method::Post, ["auth", "login"]) => login(&request),
            (Method::Get, ["auth", "me"]) => me(&request),
            (Method::Post, ["auth", "logout"]) => logout(&request),

            (Method::Post, ["api", "projects"]) => create_project(&request),
            (Method::Get, ["api", "projects"]) => list_projects(&request),
            (Method::Post, ["api", "projects", pk, "members"]) => add_member(&request, pk),

            (Method::Post, ["api", "issues"]) => create_issue(&request),
            (Method::Get, ["api", "issues"]) => list_issues(&request),
            (Method::Get, ["api", "issues", id]) => get_issue(&request, id),
            (Method::Post, ["api", "issues", id, "move"]) => move_issue(&request, id),
            (Method::Post, ["api", "issues", id, "comments"]) => add_comment(&request, id),
            (Method::Post, ["api", "issues", id, "summarize"]) => summarize(&request, id),

            (Method::Get, ["api", "search"]) => do_search(&request, &path),
            (Method::Post, ["api", "tick"]) => tick(),
            // non-API GET -> the baked SPA (index.html fallback for client routes).
            (Method::Get, _) => serve_static(&route),
            _ => Outcome::NotFound,
        };
        emit(response_out, result);
    }
}

enum Outcome {
    Json(u16, String),
    /// Raw bytes + content-type (the served SPA assets).
    Raw(u16, String, Vec<u8>),
    Auth(AuthError),
    Bad(String),
    Err(u16, String),
    Forbidden(String),
    NotFound,
}

/// Serve the baked SPA via ui:assets: exact path, else fall back to index.html
/// so client-side routes render the shell. API routes matched earlier.
fn serve_static(route: &str) -> Outcome {
    let want = if route == "/" { "/index.html" } else { route };
    match statics::get(want).or_else(|| statics::get("/index.html")) {
        Some(a) => Outcome::Raw(200, a.content_type, a.body),
        None => Outcome::NotFound,
    }
}

fn now() -> u64 {
    wall_clock::now().seconds
}

fn usage_json() -> Outcome {
    Outcome::Json(
        200,
        json!({
            "service": "track",
            "about": "a Linear-lite project tracker — auth+RBAC, search, an SSE activity feed, background sweeps, signed webhooks, and AI thread-summary",
            "auth": "POST /auth/register|login, GET /auth/me, POST /auth/logout",
            "projects": "POST|GET /api/projects, POST /api/projects/{pk}/members {subject,role}",
            "issues": "POST|GET /api/issues, GET /api/issues/{id}, POST /api/issues/{id}/move {event}, POST /api/issues/{id}/comments {body}",
            "ai": "POST /api/issues/{id}/summarize",
            "search": "GET /api/search?q=&limit=",
            "stream": "GET /api/stream?after=seq  (text/event-stream)",
            "tick": "POST /api/tick  (background stale-issue sweep)"
        })
        .to_string(),
    )
}

// ---- seeding -----------------------------------------------------------------

/// Idempotent: define the issue lifecycle machine + the project-write policy
/// (a member OR lead of the project may write its issues).
fn ensure_seeded() {
    if records::count("meta").map(|n| n > 0).unwrap_or(false) {
        return;
    }
    fn t(event: &str, source: &str, target: &str) -> fsm::Transition {
        fsm::Transition { event: event.into(), source: source.into(), target: target.into() }
    }
    let def = fsm::Definition {
        states: ["backlog", "todo", "in_progress", "done"].iter().map(|s| s.to_string()).collect(),
        initial: "backlog".into(),
        transitions: vec![
            t("start", "backlog", "todo"),
            t("begin", "todo", "in_progress"),
            t("finish", "in_progress", "done"),
            t("reopen", "done", "todo"),
            // allow jumping back a step
            t("stop", "in_progress", "todo"),
            t("shelve", "todo", "backlog"),
        ],
        terminal: vec![],
    };
    let _ = fsm::define(MACHINE, &def);

    // policy: a principal whose `role` attr is member|lead for the project may write.
    let rule = policy::Rule {
        id: "member-can-write".into(),
        action: "write".into(),
        effect: policy::Effect::Allow,
        conditions: vec![policy::Condition {
            left: "principal.role".into(),
            op: policy::Op::InList,
            right: "member,lead".into(),
        }],
        priority: 10,
    };
    let _ = policy::set_rules(POLICY_DOMAIN, &[rule]);

    let _ = records::create("meta", "{\"seeded\":true}", &[]);
}

// ---- auth --------------------------------------------------------------------

#[derive(Deserialize)]
struct RegisterReq {
    email: String,
    password: String,
    #[serde(default)]
    role: Option<String>,
}

fn register(request: &IncomingRequest) -> Outcome {
    ensure_seeded();
    let req: RegisterReq = match parse(request) {
        Ok(v) => v,
        Err(m) => return Outcome::Bad(m),
    };
    let principal = match accounts::register(&req.email, &req.password, TENANT) {
        Ok(p) => p,
        Err(e) => return Outcome::Auth(e),
    };
    // global role: `admin` may create projects; everyone else is `member`.
    let wanted = req.role.unwrap_or_else(|| "member".into());
    let role = if ["member", "admin"].contains(&wanted.as_str()) { wanted } else { "member".into() };
    let _ = rbac::assign_role(&principal.tenant, &principal.subject, &role);
    Outcome::Json(201, json!({"subject": principal.subject, "roles": [role]}).to_string())
}

#[derive(Deserialize)]
struct LoginReq {
    email: String,
    password: String,
}

fn login(request: &IncomingRequest) -> Outcome {
    let req: LoginReq = match parse(request) {
        Ok(v) => v,
        Err(m) => return Outcome::Bad(m),
    };
    match accounts::login(&req.email, &req.password, TENANT) {
        Ok(tp) => Outcome::Json(
            200,
            json!({"access_token": tp.access_token, "refresh_token": tp.refresh_token, "expires_in": tp.expires_in, "session_id": tp.session_id}).to_string(),
        ),
        Err(e) => Outcome::Auth(e),
    }
}

fn me(request: &IncomingRequest) -> Outcome {
    match introspect(request) {
        Ok(p) => Outcome::Json(200, json!({"subject": p.subject, "tenant": p.tenant, "roles": p.roles}).to_string()),
        Err(o) => o,
    }
}

fn logout(request: &IncomingRequest) -> Outcome {
    let Some(token) = bearer(request) else {
        return Outcome::Auth(AuthError::InvalidToken("missing bearer".into()));
    };
    match session::revoke(&token) {
        Ok(()) => Outcome::Json(204, String::new()),
        Err(e) => Outcome::Auth(e),
    }
}

fn introspect(request: &IncomingRequest) -> Result<Principal, Outcome> {
    let Some(token) = bearer(request) else {
        return Err(Outcome::Auth(AuthError::InvalidToken("missing bearer".into())));
    };
    authorizer::introspect(&token).map_err(Outcome::Auth)
}

fn is_admin(p: &Principal) -> bool {
    p.roles.iter().any(|r| r == "admin")
}

/// Per-project write check: look up the caller's membership row, then ask
/// policy:guard whether that role may `write`. Admins always may.
fn may_write_project(p: &Principal, project: &str) -> bool {
    if is_admin(p) {
        return true;
    }
    let role = member_role(p, project);
    if role.is_empty() {
        return false;
    }
    // attr keys are the bare names; policy resolves a condition's `principal.role`
    // reference by stripping the prefix and looking up `role` here.
    policy::enforce(
        POLICY_DOMAIN,
        "write",
        &[policy::Attr { key: "role".into(), value: role }],
        &[policy::Attr { key: "project".into(), value: project.into() }],
    )
}

/// The caller's role in a project ("lead"|"member") or "" if not a member.
fn member_role(p: &Principal, project: &str) -> String {
    let key = format!("{project}:{}", p.subject);
    records::find_by(MEMBERS, "key", &json!(key).to_string())
        .ok()
        .and_then(|entries| entries.into_iter().next())
        .and_then(|e| serde_json::from_str::<Value>(&e.data).ok())
        .and_then(|v| v["role"].as_str().map(String::from))
        .unwrap_or_default()
}

// ---- projects ----------------------------------------------------------------

fn create_project(request: &IncomingRequest) -> Outcome {
    ensure_seeded();
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    if !is_admin(&p) {
        return Outcome::Forbidden("only admins create projects".into());
    }
    #[derive(Deserialize)]
    struct Req {
        key: String,
        name: String,
    }
    let req: Req = match parse(request) {
        Ok(v) => v,
        Err(m) => return Outcome::Bad(m),
    };
    let key = req.key.trim().to_uppercase();
    if key.is_empty() || key.len() > 8 || !key.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Outcome::Bad("key must be 1..8 alphanumeric chars".into());
    }
    if req.name.trim().is_empty() {
        return Outcome::Bad("name required".into());
    }
    if !records::find_by(PROJECTS, "key", &json!(key).to_string()).unwrap_or_default().is_empty() {
        return Outcome::Err(409, "project key already exists".into());
    }
    let data = json!({"key": key, "name": req.name.trim(), "lead": p.subject, "counter": 0});
    let entry = match records::create(PROJECTS, &data.to_string(), &["key".to_string()]) {
        Ok(e) => e,
        Err(e) => return store_err(e),
    };
    // the creator is the project lead.
    add_member_row(&entry.id, &p.subject, "lead");
    publish("project.created", &json!({"project": entry.id, "key": key, "by": p.subject}));
    Outcome::Json(201, json!({"id": entry.id, "key": key, "name": req.name.trim()}).to_string())
}

fn list_projects(request: &IncomingRequest) -> Outcome {
    if let Err(o) = introspect(request) {
        return o;
    }
    match records::list_records(PROJECTS, 0, "") {
        Ok(page) => {
            let out: Vec<Value> = page.entries.iter().filter_map(|e| {
                let d: Value = serde_json::from_str(&e.data).ok()?;
                Some(json!({"id": e.id, "key": d["key"], "name": d["name"], "lead": d["lead"]}))
            }).collect();
            Outcome::Json(200, json!({"projects": out}).to_string())
        }
        Err(e) => store_err(e),
    }
}

fn add_member(request: &IncomingRequest, pk: &str) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    // only an admin or the project lead adds members.
    if !is_admin(&p) && member_role(&p, pk) != "lead" {
        return Outcome::Forbidden("only an admin or the project lead adds members".into());
    }
    if records::get(PROJECTS, pk).is_err() {
        return Outcome::NotFound;
    }
    #[derive(Deserialize)]
    struct Req {
        subject: String,
        #[serde(default)]
        role: Option<String>,
    }
    let req: Req = match parse(request) {
        Ok(v) => v,
        Err(m) => return Outcome::Bad(m),
    };
    let role = req.role.unwrap_or_else(|| "member".into());
    if !["member", "lead"].contains(&role.as_str()) {
        return Outcome::Bad("role must be member|lead".into());
    }
    add_member_row(pk, &req.subject, &role);
    Outcome::Json(201, json!({"project": pk, "subject": req.subject, "role": role}).to_string())
}

fn add_member_row(project: &str, subject: &str, role: &str) {
    let key = format!("{project}:{subject}");
    // replace an existing membership if present.
    if let Ok(entries) = records::find_by(MEMBERS, "key", &json!(key).to_string()) {
        if let Some(e) = entries.into_iter().next() {
            let data = json!({"key": key, "project": project, "subject": subject, "role": role});
            let _ = records::update(MEMBERS, &e.id, &data.to_string(), e.revision);
            return;
        }
    }
    let data = json!({"key": key, "project": project, "subject": subject, "role": role});
    let _ = records::create(MEMBERS, &data.to_string(), &["key".to_string(), "project".to_string()]);
}

// ---- issues ------------------------------------------------------------------

fn create_issue(request: &IncomingRequest) -> Outcome {
    ensure_seeded();
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    #[derive(Deserialize)]
    struct Req {
        project: String,
        title: String,
        #[serde(default)]
        body: String,
        #[serde(default)]
        label: Option<String>,
        #[serde(default)]
        assignee: Option<String>,
    }
    let req: Req = match parse(request) {
        Ok(v) => v,
        Err(m) => return Outcome::Bad(m),
    };
    let project = match records::get(PROJECTS, &req.project) {
        Ok(e) => e,
        Err(records::StoreError::NotFound) => return Outcome::Bad("unknown project".into()),
        Err(e) => return store_err(e),
    };
    if !may_write_project(&p, &req.project) {
        return Outcome::Forbidden("not a member of this project".into());
    }
    if req.title.trim().is_empty() || req.title.len() > 200 {
        return Outcome::Bad("title must be 1..200 chars".into());
    }
    // per-project issue number.
    let mut proj: Value = serde_json::from_str(&project.data).unwrap_or(Value::Null);
    let num = proj["counter"].as_u64().unwrap_or(0) + 1;
    proj["counter"] = json!(num);
    let _ = records::update(PROJECTS, &project.id, &proj.to_string(), project.revision);
    let reference = format!("{}-{num}", proj["key"].as_str().unwrap_or("ISS"));

    let label = req.label.unwrap_or_default();
    let data = json!({
        "ref": reference,
        "project": req.project,
        "title": req.title.trim(),
        "body": req.body,
        "label": label,
        "assignee": req.assignee.unwrap_or_default(),
        "reporter": p.subject,
        "status": "backlog",
        "flagged": false,
    });
    let entry = match records::create(ISSUES, &data.to_string(), &["project".to_string(), "status".to_string()]) {
        Ok(e) => e,
        Err(e) => return store_err(e),
    };
    let _ = fsm::create_instance(MACHINE, &entry.id);
    // index for search: title + body, faceted by project + label.
    let mut tags = vec![format!("project:{}", req.project)];
    if !label.is_empty() {
        tags.push(format!("label:{label}"));
    }
    let _ = search::index_doc(&entry.id, &format!("{} {}", req.title, req.body), &tags);
    publish("issue.created", &json!({"issue": entry.id, "ref": reference, "project": req.project, "by": p.subject}));
    Outcome::Json(201, issue_json(&entry).to_string())
}

fn list_issues(request: &IncomingRequest, ) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let _ = p;
    // filters come from the query string on the ORIGINAL path.
    let path = request.path_with_query().unwrap_or_default();
    let project = query_str(&path, "project");
    let status = query_str(&path, "status");
    let requested = query_i64(&path, "limit").unwrap_or(20) as u32;
    let limit = paginate::clamp_limit(requested).unwrap_or(20);
    let after = match query_str(&path, "after") {
        Some(c) if !c.is_empty() => match paginate::decode(&c) {
            Ok(pos) => pos.last_id,
            Err(_) => return Outcome::Bad("invalid cursor".into()),
        },
        _ => String::new(),
    };

    // narrow by an index when a filter is given, else page the collection.
    let entries = if let Some(pr) = &project {
        match records::find_by(ISSUES, "project", &json!(pr).to_string()) {
            Ok(e) => e,
            Err(e) => return store_err(e),
        }
    } else {
        match records::list_records(ISSUES, limit, &after) {
            Ok(page) => page.entries,
            Err(e) => return store_err(e),
        }
    };
    let out: Vec<Value> = entries
        .iter()
        .filter(|e| {
            status.as_ref().map_or(true, |s| {
                serde_json::from_str::<Value>(&e.data).ok().and_then(|d| d["status"].as_str().map(|x| x == s)).unwrap_or(false)
            })
        })
        .map(issue_json)
        .collect();
    Outcome::Json(200, json!({"issues": out}).to_string())
}

fn get_issue(request: &IncomingRequest, id: &str) -> Outcome {
    if let Err(o) = introspect(request) {
        return o;
    }
    let entry = match records::get(ISSUES, id) {
        Ok(e) => e,
        Err(records::StoreError::NotFound) => return Outcome::NotFound,
        Err(e) => return store_err(e),
    };
    let comments = records::find_by(COMMENTS, "issue", &json!(id).to_string()).unwrap_or_default();
    let comments: Vec<Value> = comments.iter().filter_map(|c| {
        let d: Value = serde_json::from_str(&c.data).ok()?;
        let body = d["body"].as_str().unwrap_or("");
        Some(json!({"id": c.id, "author": d["author"], "body": body, "html": md::to_html(body), "at": c.created}))
    }).collect();
    let mut out = issue_json(&entry);
    out["comments"] = json!(comments);
    out["allowed_events"] = json!(fsm::allowed_events(MACHINE, id).unwrap_or_default());
    Outcome::Json(200, out.to_string())
}

fn move_issue(request: &IncomingRequest, id: &str) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let entry = match records::get(ISSUES, id) {
        Ok(e) => e,
        Err(records::StoreError::NotFound) => return Outcome::NotFound,
        Err(e) => return store_err(e),
    };
    let data: Value = serde_json::from_str(&entry.data).unwrap_or(Value::Null);
    let project = data["project"].as_str().unwrap_or("");
    if !may_write_project(&p, project) {
        return Outcome::Forbidden("not a member of this project".into());
    }
    #[derive(Deserialize)]
    struct Req {
        event: String,
    }
    let req: Req = match parse(request) {
        Ok(v) => v,
        Err(m) => return Outcome::Bad(m),
    };
    match fsm::fire(MACHINE, id, &req.event) {
        Ok(status) => {
            mirror_status(&entry, &data, &status.state);
            publish("issue.moved", &json!({"issue": id, "ref": data["ref"], "event": req.event, "to": status.state, "by": p.subject}));
            // out axis: signed webhook on every transition.
            fire_webhook("issue.moved", &json!({"issue": id, "ref": data["ref"], "to": status.state}));
            Outcome::Json(200, json!({"status": status.state, "done": status.done}).to_string())
        }
        Err(fsm::FsmError::IllegalTransition(cur)) => Outcome::Err(409, format!("cannot {} from {cur}", req.event)),
        Err(e) => Outcome::Err(503, format!("fsm: {e:?}")),
    }
}

fn add_comment(request: &IncomingRequest, id: &str) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let entry = match records::get(ISSUES, id) {
        Ok(e) => e,
        Err(records::StoreError::NotFound) => return Outcome::NotFound,
        Err(e) => return store_err(e),
    };
    let data: Value = serde_json::from_str(&entry.data).unwrap_or(Value::Null);
    let project = data["project"].as_str().unwrap_or("");
    if !may_write_project(&p, project) {
        return Outcome::Forbidden("not a member of this project".into());
    }
    #[derive(Deserialize)]
    struct Req {
        body: String,
    }
    let req: Req = match parse(request) {
        Ok(v) => v,
        Err(m) => return Outcome::Bad(m),
    };
    if req.body.trim().is_empty() || req.body.len() > 10_000 {
        return Outcome::Bad("body must be 1..10000 chars".into());
    }
    let c = json!({"issue": id, "author": p.subject, "body": req.body});
    let created = match records::create(COMMENTS, &c.to_string(), &["issue".to_string()]) {
        Ok(e) => e,
        Err(e) => return store_err(e),
    };
    publish("comment.added", &json!({"issue": id, "ref": data["ref"], "by": p.subject}));
    Outcome::Json(201, json!({"id": created.id, "html": md::to_html(&req.body)}).to_string())
}

// ---- AI: summarize the comment thread ----------------------------------------

fn summarize(request: &IncomingRequest, id: &str) -> Outcome {
    if let Err(o) = introspect(request) {
        return o;
    }
    let entry = match records::get(ISSUES, id) {
        Ok(e) => e,
        Err(records::StoreError::NotFound) => return Outcome::NotFound,
        Err(e) => return store_err(e),
    };
    let data: Value = serde_json::from_str(&entry.data).unwrap_or(Value::Null);
    let comments = records::find_by(COMMENTS, "issue", &json!(id).to_string()).unwrap_or_default();
    // assemble the thread text: title + body + every comment.
    let mut thread = format!("{}\n{}\n", data["title"].as_str().unwrap_or(""), data["body"].as_str().unwrap_or(""));
    for c in &comments {
        if let Ok(d) = serde_json::from_str::<Value>(&c.data) {
            thread.push_str(d["body"].as_str().unwrap_or(""));
            thread.push('\n');
        }
    }
    match ai::summarize(&thread, ai::Length::Brief, "status and next steps") {
        Ok(summary) => Outcome::Json(200, json!({"issue": id, "summary": summary, "comments": comments.len()}).to_string()),
        Err(e) => Outcome::Err(503, format!("ai: {e:?}")),
    }
}

// ---- read: search ------------------------------------------------------------

fn do_search(request: &IncomingRequest, path: &str) -> Outcome {
    if let Err(o) = introspect(request) {
        return o;
    }
    let q = query_str(path, "q").unwrap_or_default();
    if q.trim().is_empty() {
        return Outcome::Json(200, json!({"hits": []}).to_string());
    }
    let limit = query_i64(path, "limit").unwrap_or(10).clamp(1, 50) as u32;
    let tags: Vec<String> = match query_str(path, "project") {
        Some(pr) if !pr.is_empty() => vec![format!("project:{pr}")],
        _ => vec![],
    };
    let hits = match search::query(q.trim(), search::Mode::Any, &tags, limit) {
        Ok(h) => h,
        Err(e) => return Outcome::Err(503, format!("search: {e:?}")),
    };
    let rows: Vec<Value> = hits.iter().filter_map(|h| {
        let e = records::get(ISSUES, &h.id).ok()?;
        let mut j = issue_json(&e);
        j["score"] = json!((h.score * 1000.0).round() / 1000.0);
        Some(j)
    }).collect();
    Outcome::Json(200, json!({"hits": rows}).to_string())
}

// ---- background: stale-issue sweep -------------------------------------------

/// The timer pump (wasip2 has no background tasks). Flag in_progress issues
/// older than STALE_SECS that aren't already flagged, publishing an event each.
fn tick() -> Outcome {
    let entries = records::find_by(ISSUES, "status", &json!("in_progress").to_string()).unwrap_or_default();
    let cutoff = now().saturating_sub(STALE_SECS);
    let mut flagged = 0;
    for e in &entries {
        if e.updated > cutoff {
            continue;
        }
        let mut d: Value = match serde_json::from_str(&e.data) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if d["flagged"].as_bool().unwrap_or(false) {
            continue;
        }
        d["flagged"] = json!(true);
        if records::update(ISSUES, &e.id, &d.to_string(), e.revision).is_ok() {
            flagged += 1;
            publish("issue.flagged", &json!({"issue": e.id, "ref": d["ref"], "reason": "stale in_progress"}));
        }
    }
    Outcome::Json(200, json!({"swept": entries.len(), "flagged": flagged}).to_string())
}

// ---- stream: SSE activity feed -----------------------------------------------

fn stream_events(response_out: ResponseOutparam, path: &str) {
    let headers = Fields::new();
    let _ = headers.set(&"content-type".to_string(), &[b"text/event-stream".to_vec()]);
    let _ = headers.set(&"cache-control".to_string(), &[b"no-cache".to_vec()]);
    let _ = headers.set(&"access-control-allow-origin".to_string(), &[b"*".to_vec()]);
    let response = OutgoingResponse::new(headers);
    let _ = response.set_status_code(200);
    let body = response.body().expect("outgoing body");
    ResponseOutparam::set(response_out, Ok(response));

    // default: only activity produced after we connect; ?after= catches up.
    let mut cursor = query_i64(path, "after").unwrap_or_else(current_seq);
    {
        let stream = body.write().expect("write stream");
        if stream.blocking_write_and_flush(b": connected\n\n").is_err() {
            return;
        }
        for _ in 0..MAX_TICKS {
            let (rows, next) = activity_after(cursor);
            cursor = next;
            let frame = if rows.is_empty() {
                ": ping\n\n".to_string()
            } else {
                rows.iter().map(|r| format!("data: {r}\n\n")).collect::<String>()
            };
            if stream.blocking_write_and_flush(frame.as_bytes()).is_err() {
                break;
            }
            monotonic_clock::subscribe_duration(POLL_MS * 1_000_000).block();
        }
    }
    let _ = OutgoingBody::finish(body, None);
}

/// Publish an activity event (the SSE spine). Best-effort — never fails a write.
fn publish(kind: &str, detail: &Value) {
    let frame = json!({"kind": kind, "detail": detail, "at": now()});
    let _ = bus::publish(ACTIVITY, frame.to_string().as_bytes());
}

/// Highest activity seq so far (the "only new" starting cursor).
fn current_seq() -> i64 {
    bus::poll(ACTIVITY, "snapshot", 4096)
        .unwrap_or_default()
        .iter()
        .filter_map(|e| e.id.parse::<i64>().ok())
        .max()
        .unwrap_or(-1)
}

/// Activity rows with seq > cursor (JSON strings), plus the new cursor.
fn activity_after(cursor: i64) -> (Vec<String>, i64) {
    let events = bus::poll(ACTIVITY, "snapshot", 4096).unwrap_or_default();
    let mut max = cursor;
    let mut rows = Vec::new();
    for e in &events {
        let seq: i64 = e.id.parse().unwrap_or(-1);
        if seq > cursor {
            if seq > max {
                max = seq;
            }
            if let Ok(s) = String::from_utf8(e.payload.clone()) {
                rows.push(s);
            }
        }
    }
    (rows, max)
}

// ---- out: signed webhook -----------------------------------------------------

/// Fire a signed outbound webhook via notify:dispatch. The target URL comes
/// from config (notify:*-url); the body carries a Stripe-style signature header.
/// Best-effort — a delivery failure never fails the request.
fn fire_webhook(event: &str, detail: &Value) {
    let body = json!({"event": event, "detail": detail, "at": now()}).to_string();
    let sig = match sign::sign(body.as_bytes(), WEBHOOK_SECRET, sign::Scheme::Stripe) {
        Ok(s) => s.header,
        Err(_) => String::new(),
    };
    // the signature travels in the body envelope for the demo (notify:dispatch's
    // webhook channel posts the raw body); a real integration puts it in a header.
    let envelope = json!({"signature": sig, "payload": body});
    let msg = notify::Message {
        channel: notify::Channel::Webhook,
        target: String::new(), // resolved from notify config by the dispatcher
        subject: event.into(),
        body: envelope.to_string(),
    };
    let _ = notify::send(&msg);
}

// ---- helpers -----------------------------------------------------------------

fn issue_json(entry: &records::Entry) -> Value {
    let d: Value = serde_json::from_str(&entry.data).unwrap_or(Value::Null);
    json!({
        "id": entry.id,
        "ref": d["ref"],
        "project": d["project"],
        "title": d["title"],
        "label": d["label"],
        "assignee": d["assignee"],
        "reporter": d["reporter"],
        "status": d["status"],
        "flagged": d["flagged"],
        "created": entry.created,
        "updated": entry.updated,
    })
}

fn mirror_status(entry: &records::Entry, data: &Value, state: &str) {
    let mut data = data.clone();
    data["status"] = json!(state);
    let _ = records::update(ISSUES, &entry.id, &data.to_string(), 0);
}

fn store_err(e: records::StoreError) -> Outcome {
    match e {
        records::StoreError::NotFound => Outcome::NotFound,
        records::StoreError::InvalidJson(m) => Outcome::Bad(m),
        records::StoreError::RevisionConflict(_) => Outcome::Err(409, "conflict".into()),
        records::StoreError::BackendUnavailable(m) => Outcome::Err(503, m),
    }
}

fn auth_error(e: &AuthError) -> (u16, &'static str) {
    match e {
        AuthError::InvalidCredentials => (401, "invalid_credentials"),
        AuthError::AlreadyExists => (409, "already_exists"),
        AuthError::RateLimited(_) => (429, "rate_limited"),
        AuthError::InsufficientScope(_) => (403, "insufficient_scope"),
        AuthError::Expired => (401, "expired"),
        AuthError::InvalidToken(_) => (401, "invalid_token"),
        AuthError::UnknownTenant => (403, "unknown_tenant"),
        AuthError::Malformed(_) => (400, "malformed"),
        AuthError::BackendUnavailable(_) => (503, "backend_unavailable"),
        AuthError::Internal(_) => (500, "internal"),
    }
}

fn parse<T: for<'a> Deserialize<'a>>(request: &IncomingRequest) -> Result<T, String> {
    let body = read_body(request).map_err(|_| "could not read body".to_string())?;
    serde_json::from_slice(&body).map_err(|e| format!("bad json: {e}"))
}

fn read_body(request: &IncomingRequest) -> Result<Vec<u8>, ()> {
    let body = request.consume().map_err(|_| ())?;
    let stream = body.stream().map_err(|_| ())?;
    let mut buf = Vec::new();
    loop {
        match stream.blocking_read(8192) {
            Ok(chunk) if chunk.is_empty() => break,
            Ok(chunk) => buf.extend_from_slice(&chunk),
            Err(_) => break,
        }
    }
    Ok(buf)
}

fn bearer(request: &IncomingRequest) -> Option<String> {
    header(request, "authorization").and_then(|s| s.strip_prefix("Bearer ").map(|t| t.trim().to_string()))
}

fn header(request: &IncomingRequest, name: &str) -> Option<String> {
    request.headers().get(&name.to_string()).into_iter().find_map(|v| String::from_utf8(v).ok())
}

fn query_str(path: &str, key: &str) -> Option<String> {
    let query = path.split('?').nth(1)?;
    query.split('&').find_map(|pair| {
        let mut it = pair.splitn(2, '=');
        (it.next()? == key).then(|| decode(it.next().unwrap_or("")))
    })
}

fn query_i64(path: &str, key: &str) -> Option<i64> {
    query_str(path, key)?.parse().ok()
}

fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    out.push(b as char);
                    i += 3;
                    continue;
                }
                out.push('%');
                i += 1;
            }
            b'+' => {
                out.push(' ');
                i += 1;
            }
            c => {
                out.push(c as char);
                i += 1;
            }
        }
    }
    out
}

fn emit(response_out: ResponseOutparam, result: Outcome) {
    match result {
        Outcome::Json(code, body) => respond_ct(response_out, code, "application/json", &[], body.as_bytes()),
        Outcome::Raw(code, ct, bytes) => respond_ct(response_out, code, &ct, &[], &bytes),
        Outcome::Auth(e) => {
            if let AuthError::RateLimited(secs) = e {
                respond_ct(response_out, 429, "application/json", &[("retry-after", &secs.to_string())], format!("{{\"error\":\"rate_limited\",\"retryAfter\":{secs}}}").as_bytes());
            } else {
                let (code, msg) = auth_error(&e);
                respond_ct(response_out, code, "application/json", &[], format!("{{\"error\":\"{msg}\"}}").as_bytes());
            }
        }
        Outcome::Bad(msg) => respond_ct(response_out, 400, "application/json", &[], json!({ "error": msg }).to_string().as_bytes()),
        Outcome::Err(code, msg) => respond_ct(response_out, code, "application/json", &[], json!({ "error": msg }).to_string().as_bytes()),
        Outcome::Forbidden(msg) => respond_ct(response_out, 403, "application/json", &[], json!({ "error": msg }).to_string().as_bytes()),
        Outcome::NotFound => respond_ct(response_out, 404, "application/json", &[], b"{\"error\":\"not_found\"}"),
    }
}

fn respond_ct(response_out: ResponseOutparam, status: u16, content_type: &str, extra: &[(&str, &str)], body: &[u8]) {
    let headers = Fields::new();
    let _ = headers.set(&"content-type".to_string(), &[content_type.as_bytes().to_vec()]);
    let _ = headers.set(&"access-control-allow-origin".to_string(), &[b"*".to_vec()]);
    for (k, v) in extra {
        let _ = headers.set(&k.to_string(), &[v.as_bytes().to_vec()]);
    }
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
