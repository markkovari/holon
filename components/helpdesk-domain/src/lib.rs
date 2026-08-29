//! helpdesk:app — support/ticketing SaaS domain over composed contracts.
//!
//! The lifecycle is a declarative fsm:workflow machine, not scattered
//! `if status == ...` checks: replies and agent verbs FIRE events, the engine
//! validates legality and keeps the history. The ticket record mirrors the
//! FSM state in a `status` field (indexed) so list/filter is a lookup, the
//! machine stays the source of truth for transitions.
//!
//! Access model (rung 1): role `agent`/`admin` sees and drives everything;
//! everyone else is a requester who sees only their own tickets and never the
//! internal notes.

#[allow(warnings)]
mod bindings;

use serde::Deserialize;
use serde_json::{json, Value};

use bindings::auth::identity::accounts;
use bindings::auth::identity::authorizer;
use bindings::auth::identity::rbac;
use bindings::auth::identity::session;
use bindings::auth::identity::types::{AuthError, Principal};
use bindings::fsm::workflow::engine as fsm;
use bindings::id::generate::generator as ids;
use bindings::md::render::renderer as md;
use bindings::records::store::store as records;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

struct Component;

const TENANT: &str = "helpdesk";
const TICKETS: &str = "tickets";
const MESSAGES: &str = "messages";
const MACHINE: &str = "ticket";
const PRIORITIES: [&str; 4] = ["low", "normal", "high", "urgent"];
const AGENT_EVENTS: [&str; 4] = ["triage", "solve", "close", "reopen"];

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let method = request.method();
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/").to_string();
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        let result = match (&method, seg.as_slice()) {
            (Method::Get, [""]) => usage_json(),
            (Method::Post, ["auth", "register"]) => register(&request),
            (Method::Post, ["auth", "login"]) => login(&request),
            (Method::Get, ["auth", "me"]) => me(&request),
            (Method::Post, ["auth", "logout"]) => logout(&request),

            (Method::Post, ["api", "tickets"]) => create_ticket(&request),
            (Method::Get, ["api", "tickets"]) => list_tickets(&request),
            (Method::Get, ["api", "tickets", id]) => get_ticket(&request, id),
            (Method::Post, ["api", "tickets", id, "messages"]) => add_message(&request, id),
            (Method::Post, ["api", "tickets", id, "state"]) => change_state(&request, id),
            (Method::Post, ["api", "tickets", id, "assign"]) => assign(&request, id),
            (Method::Get, ["api", "tickets", id, "history"]) => ticket_history(&request, id),
            _ => Outcome::NotFound,
        };
        emit(response_out, result);
    }
}

enum Outcome {
    Json(u16, String),
    Auth(AuthError),
    Bad(String),
    Err(u16, String),
    Forbidden(String),
    NotFound,
}

fn usage_json() -> Outcome {
    Outcome::Json(
        200,
        json!({
            "service": "helpdesk",
            "auth": "POST /auth/register|login, GET /auth/me, POST /auth/logout",
            "tickets": "POST|GET /api/tickets, GET /api/tickets/{id}",
            "messages": "POST /api/tickets/{id}/messages {body, internal?}",
            "lifecycle": "POST /api/tickets/{id}/state {event: triage|solve|close|reopen}",
            "assign": "POST /api/tickets/{id}/assign {subject}",
            "history": "GET /api/tickets/{id}/history"
        })
        .to_string(),
    )
}

// ---- seeding ---------------------------------------------------------------

/// Idempotent: register the ticket lifecycle machine. Gated on one record so
/// steady-state requests pay a single count() read.
fn ensure_seeded() {
    if records::count("meta").map(|n| n > 0).unwrap_or(false) {
        return;
    }
    fn t(event: &str, source: &str, target: &str) -> fsm::Transition {
        fsm::Transition { event: event.into(), source: source.into(), target: target.into() }
    }
    let def = fsm::Definition {
        states: ["new", "open", "pending", "solved", "closed"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        initial: "new".into(),
        transitions: vec![
            t("triage", "new", "open"),
            t("reply", "open", "pending"),
            t("requester-reply", "pending", "open"),
            t("reopen", "solved", "open"),
            t("solve", "new", "solved"),
            t("solve", "open", "solved"),
            t("solve", "pending", "solved"),
            t("close", "solved", "closed"),
        ],
        terminal: vec!["closed".into()],
    };
    let _ = fsm::define(MACHINE, &def);
    let _ = records::create("meta", "{\"seeded\":true}", &[]);
}

// ---- auth ------------------------------------------------------------------

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
    let wanted = req.role.unwrap_or_else(|| "requester".into());
    let role = if ["requester", "agent", "admin"].contains(&wanted.as_str()) {
        wanted
    } else {
        "requester".into()
    };
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
            json!({
                "access_token": tp.access_token,
                "refresh_token": tp.refresh_token,
                "expires_in": tp.expires_in,
                "session_id": tp.session_id,
            })
            .to_string(),
        ),
        Err(e) => Outcome::Auth(e),
    }
}

fn me(request: &IncomingRequest) -> Outcome {
    match introspect(request) {
        Ok(p) => Outcome::Json(
            200,
            json!({"subject": p.subject, "tenant": p.tenant, "roles": p.roles}).to_string(),
        ),
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

fn is_agent(p: &Principal) -> bool {
    p.roles.iter().any(|r| r == "agent" || r == "admin")
}

/// Load a ticket; requesters only see their own (404, not 403 — existence is
/// not leaked across requesters).
fn load_ticket(p: &Principal, id: &str) -> Result<(records::Entry, Value), Outcome> {
    let entry = match records::get(TICKETS, id) {
        Ok(e) => e,
        Err(records::StoreError::NotFound) => return Err(Outcome::NotFound),
        Err(e) => return Err(store_err(e)),
    };
    let data: Value = serde_json::from_str(&entry.data).unwrap_or(Value::Null);
    if !is_agent(p) && data["requester"].as_str() != Some(p.subject.as_str()) {
        return Err(Outcome::NotFound);
    }
    Ok((entry, data))
}

// ---- tickets -----------------------------------------------------------------

fn create_ticket(request: &IncomingRequest) -> Outcome {
    ensure_seeded();
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    #[derive(Deserialize)]
    struct Req {
        subject: String,
        body: String,
        #[serde(default)]
        priority: Option<String>,
    }
    let req: Req = match parse(request) {
        Ok(v) => v,
        Err(m) => return Outcome::Bad(m),
    };
    if req.subject.is_empty() || req.subject.len() > 200 {
        return Outcome::Bad("subject must be 1..200 chars".into());
    }
    if req.body.is_empty() || req.body.len() > 10_000 {
        return Outcome::Bad("body must be 1..10000 chars".into());
    }
    let priority = req.priority.unwrap_or_else(|| "normal".into());
    if !PRIORITIES.contains(&priority.as_str()) {
        return Outcome::Bad(format!("priority must be one of {PRIORITIES:?}"));
    }
    let data = json!({
        "ref": format!("HD-{}", ids::short_code(6)),
        "subject": req.subject,
        "requester": p.subject,
        "assignee": "",
        "priority": priority,
        "status": "new",
    });
    let entry = match records::create(
        TICKETS,
        &data.to_string(),
        &["requester".to_string(), "status".to_string()],
    ) {
        Ok(e) => e,
        Err(e) => return store_err(e),
    };
    let _ = fsm::create_instance(MACHINE, &entry.id);
    let msg = json!({"ticket": entry.id, "author": p.subject, "kind": "public", "body": req.body});
    if let Err(e) = records::create(MESSAGES, &msg.to_string(), &["ticket".to_string()]) {
        return store_err(e);
    }
    Outcome::Json(201, ticket_json(&entry).to_string())
}

fn list_tickets(request: &IncomingRequest) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let entries = if is_agent(&p) {
        match records::list_records(TICKETS, 0, "") {
            Ok(page) => page.entries,
            Err(e) => return store_err(e),
        }
    } else {
        match records::find_by(TICKETS, "requester", &json!(p.subject).to_string()) {
            Ok(entries) => entries,
            Err(e) => return store_err(e),
        }
    };
    let tickets: Vec<Value> = entries.iter().map(ticket_json).collect();
    Outcome::Json(200, json!({ "tickets": tickets }).to_string())
}

fn get_ticket(request: &IncomingRequest, id: &str) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let (entry, _) = match load_ticket(&p, id) {
        Ok(t) => t,
        Err(o) => return o,
    };
    let agent = is_agent(&p);
    let messages = match records::find_by(MESSAGES, "ticket", &json!(id).to_string()) {
        Ok(entries) => entries,
        Err(e) => return store_err(e),
    };
    let messages: Vec<Value> = messages
        .iter()
        .filter_map(|m| {
            let d: Value = serde_json::from_str(&m.data).unwrap_or(Value::Null);
            if !agent && d["kind"] == "internal" {
                return None;
            }
            let body = d["body"].as_str().unwrap_or("");
            Some(json!({
                "id": m.id,
                "author": d["author"],
                "kind": d["kind"],
                "body": body,
                "html": md::to_html(body),
                "created": m.created,
            }))
        })
        .collect();
    let mut out = ticket_json(&entry);
    out["messages"] = json!(messages);
    if agent {
        out["allowed_events"] = json!(fsm::allowed_events(MACHINE, id).unwrap_or_default());
    }
    Outcome::Json(200, out.to_string())
}

// ---- messages + lifecycle -----------------------------------------------------

fn add_message(request: &IncomingRequest, id: &str) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let (entry, data) = match load_ticket(&p, id) {
        Ok(t) => t,
        Err(o) => return o,
    };
    if data["status"] == "closed" {
        return Outcome::Err(409, "ticket is closed".into());
    }
    #[derive(Deserialize)]
    struct Req {
        body: String,
        #[serde(default)]
        internal: bool,
    }
    let req: Req = match parse(request) {
        Ok(v) => v,
        Err(m) => return Outcome::Bad(m),
    };
    let agent = is_agent(&p);
    if req.internal && !agent {
        return Outcome::Forbidden("internal notes are agent-only".into());
    }
    if req.body.is_empty() || req.body.len() > 10_000 {
        return Outcome::Bad("body must be 1..10000 chars".into());
    }
    let kind = if req.internal { "internal" } else { "public" };
    let msg = json!({"ticket": id, "author": p.subject, "kind": kind, "body": req.body});
    let created = match records::create(MESSAGES, &msg.to_string(), &["ticket".to_string()]) {
        Ok(e) => e,
        Err(e) => return store_err(e),
    };
    // Which lifecycle events a message implies. Internal notes move nothing.
    let events: &[&str] = match (agent, req.internal, data["status"].as_str().unwrap_or("")) {
        (_, true, _) => &[],
        // agent public reply: triage first if still new, then wait on requester.
        (true, _, "new") => &["triage", "reply"],
        (true, _, _) => &["reply"],
        // requester reply: hand back to agents / reopen a solved ticket.
        (false, _, "pending") => &["requester-reply"],
        (false, _, "solved") => &["reopen"],
        (false, _, _) => &[],
    };
    let status = apply_events(&entry, &data, events)
        .unwrap_or_else(|| data["status"].as_str().unwrap_or("").into());
    Outcome::Json(201, json!({"id": created.id, "kind": kind, "status": status}).to_string())
}

fn change_state(request: &IncomingRequest, id: &str) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    if !is_agent(&p) {
        return Outcome::Forbidden("lifecycle events are agent-only".into());
    }
    let (entry, data) = match load_ticket(&p, id) {
        Ok(t) => t,
        Err(o) => return o,
    };
    #[derive(Deserialize)]
    struct Req {
        event: String,
    }
    let req: Req = match parse(request) {
        Ok(v) => v,
        Err(m) => return Outcome::Bad(m),
    };
    if !AGENT_EVENTS.contains(&req.event.as_str()) {
        return Outcome::Bad(format!("event must be one of {AGENT_EVENTS:?}"));
    }
    match fsm::fire(MACHINE, id, &req.event) {
        Ok(status) => {
            mirror_status(&entry, &data, &status.state);
            Outcome::Json(200, json!({"status": status.state, "done": status.done}).to_string())
        }
        Err(fsm::FsmError::IllegalTransition(current)) => {
            Outcome::Err(409, format!("cannot {} from {current}", req.event))
        }
        Err(e) => Outcome::Err(503, format!("fsm: {e:?}")),
    }
}

fn assign(request: &IncomingRequest, id: &str) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    if !is_agent(&p) {
        return Outcome::Forbidden("assignment is agent-only".into());
    }
    let (entry, mut data) = match load_ticket(&p, id) {
        Ok(t) => t,
        Err(o) => return o,
    };
    #[derive(Deserialize)]
    struct Req {
        subject: String,
    }
    let req: Req = match parse(request) {
        Ok(v) => v,
        Err(m) => return Outcome::Bad(m),
    };
    data["assignee"] = json!(req.subject);
    // taking a ticket triages it out of `new`.
    if data["status"] == "new" {
        if let Ok(status) = fsm::fire(MACHINE, id, "triage") {
            data["status"] = json!(status.state);
        }
    }
    match records::update(TICKETS, id, &data.to_string(), entry.revision) {
        Ok(e) => Outcome::Json(200, ticket_json(&e).to_string()),
        Err(e) => store_err(e),
    }
}

fn ticket_history(request: &IncomingRequest, id: &str) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    if let Err(o) = load_ticket(&p, id) {
        return o;
    }
    match fsm::history(MACHINE, id) {
        Ok(entries) => {
            let history: Vec<Value> = entries
                .iter()
                .map(|h| json!({"event": h.event, "from": h.source, "to": h.target, "at": h.at}))
                .collect();
            Outcome::Json(200, json!({ "history": history }).to_string())
        }
        Err(e) => Outcome::Err(503, format!("fsm: {e:?}")),
    }
}

/// Fire `events` in order (illegal ones are skipped — the machine decides),
/// mirror the final state onto the ticket record. Returns the new state if
/// anything moved.
fn apply_events(entry: &records::Entry, data: &Value, events: &[&str]) -> Option<String> {
    let mut latest = None;
    for ev in events {
        if let Ok(status) = fsm::fire(MACHINE, &entry.id, ev) {
            latest = Some(status.state);
        }
    }
    if let Some(state) = &latest {
        mirror_status(entry, data, state);
    }
    latest
}

fn mirror_status(entry: &records::Entry, data: &Value, state: &str) {
    let mut data = data.clone();
    data["status"] = json!(state);
    // revision 0 = last-write-wins; the FSM already serialized the transition.
    let _ = records::update(TICKETS, &entry.id, &data.to_string(), 0);
}

// ---- helpers ---------------------------------------------------------------------

fn ticket_json(entry: &records::Entry) -> Value {
    let data: Value = serde_json::from_str(&entry.data).unwrap_or(Value::Null);
    json!({
        "id": entry.id,
        "ref": data["ref"],
        "subject": data["subject"],
        "requester": data["requester"],
        "assignee": data["assignee"],
        "priority": data["priority"],
        "status": data["status"],
        "created": entry.created,
        "updated": entry.updated,
    })
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

fn bearer(request: &IncomingRequest) -> Option<String> {
    header(request, "authorization")
        .and_then(|s| s.strip_prefix("Bearer ").map(|tok| tok.trim().to_string()))
}

fn header(request: &IncomingRequest, name: &str) -> Option<String> {
    request.headers().get(name).into_iter().find_map(|v| String::from_utf8(v).ok())
}

// ---- responses --------------------------------------------------------------------

fn emit(response_out: ResponseOutparam, result: Outcome) {
    match result {
        Outcome::Json(code, body) => respond(response_out, code, &[], body.as_bytes()),
        Outcome::Auth(e) => {
            if let AuthError::RateLimited(secs) = e {
                respond(
                    response_out,
                    429,
                    &[("retry-after", &secs.to_string())],
                    format!("{{\"error\":\"rate_limited\",\"retryAfter\":{secs}}}").as_bytes(),
                );
            } else {
                let (code, msg) = auth_error(&e);
                respond(response_out, code, &[], format!("{{\"error\":\"{msg}\"}}").as_bytes());
            }
        }
        Outcome::Bad(msg) => {
            respond(response_out, 400, &[], json!({ "error": msg }).to_string().as_bytes())
        }
        Outcome::Err(code, msg) => {
            respond(response_out, code, &[], json!({ "error": msg }).to_string().as_bytes())
        }
        Outcome::Forbidden(msg) => {
            respond(response_out, 403, &[], json!({ "error": msg }).to_string().as_bytes())
        }
        Outcome::NotFound => respond(response_out, 404, &[], b"{\"error\":\"not_found\"}"),
    }
}

fn respond(response_out: ResponseOutparam, status: u16, extra: &[(&str, &str)], body: &[u8]) {
    let headers = Fields::new();
    let _ = headers.set("content-type", &[b"application/json".to_vec()]);
    for (k, v) in extra {
        let _ = headers.set(k.as_ref(), &[v.as_bytes().to_vec()]);
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
