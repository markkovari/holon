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
use bindings::ratelimit::guard::limiter as ratelimit;
use bindings::ratelimit::guard::limiter::LimitError;
use bindings::quota::meter::meter as quota;
use bindings::quota::meter::meter::QuotaError;
use bindings::policy::guard::guard as policy;
use bindings::policy::guard::guard::Attr;
use bindings::audit::log::recorder as audit;
use bindings::audit::log::types::Event;
use bindings::event::bus::bus as eventbus;
use bindings::notify::dispatch::dispatcher as notify;
use bindings::email::template::renderer as email;
use bindings::i18n::catalog::catalog as i18n;
use bindings::idempotency::guard::store as idempotency;
use bindings::webhook::sign::signer as webhook_sign;
use bindings::outbox::dispatch::queue as outbox;
use bindings::webhook::ingest::verifier as webhook_ingest;
use bindings::mail::parse::parser as mail_parse;
use bindings::upload::policy::gate as upload_policy;
use bindings::blob::store::blobstore as blob_store;
use bindings::sched::timer::timer as timer;
use bindings::wasi::clocks::wall_clock;
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
            (Method::Post, ["api", "ops", "process-events"]) => process_events(&request),
            (Method::Post, ["api", "webhooks", "email"]) => ingest_email(&request),

            (Method::Post, ["api", "tickets"]) => create_ticket(&request),
            (Method::Get, ["api", "tickets"]) => list_tickets(&request),
            (Method::Get, ["api", "tickets", "search"]) => search_tickets(&request),
            (Method::Get, ["api", "tickets", id]) => get_ticket(&request, id),
            (Method::Post, ["api", "tickets", id, "messages"]) => add_message(&request, id),
            (Method::Post, ["api", "tickets", id, "state"]) => change_state(&request, id),
            (Method::Post, ["api", "tickets", id, "assign"]) => assign(&request, id),
            (Method::Get, ["api", "tickets", id, "history"]) => ticket_history(&request, id),
            (Method::Post, ["api", "internal", "timers", "fire"]) => timers_fire(&request),
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

fn is_allowed(p: &Principal, action: &str, ticket_id: &str, status: &str) -> bool {
    let principal_attrs = vec![
        Attr { key: "subject".into(), value: p.subject.clone() },
        Attr { key: "tenant".into(), value: p.tenant.clone() },
        Attr { key: "roles".into(), value: p.roles.join(",") },
    ];
    let target_attrs = vec![
        Attr { key: "id".into(), value: ticket_id.into() },
        Attr { key: "status".into(), value: status.into() },
    ];
    policy::enforce("helpdesk", action, &principal_attrs, &target_attrs)
}

fn audit_log(p: &Principal, action: &str, target: &str, outcome: &str) {
    let e = Event {
        id: "".into(),
        trace_id: "".into(),
        span_id: "".into(),
        timestamp: 0,
        event: action.into(),
        outcome: outcome.into(),
        tenant: p.tenant.clone(),
        subject: p.subject.clone(),
        detail: target.into(),
    };
    let _ = audit::record_event(&e);
}

// ---- event processor ---------------------------------------------------------

fn process_events(request: &IncomingRequest) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    if !is_agent(&p) {
        return Outcome::Forbidden("only agents can process events".into());
    }

    let events = match eventbus::poll("helpdesk.events", "helpdesk_fanout", 50) {
        Ok(evs) => evs,
        Err(e) => return Outcome::Err(503, format!("eventbus error: {:?}", e)),
    };

    let mut ack_ids = Vec::new();

    for ev in events {
        let idempotency_key = format!("fanout:{}", ev.id);
        match idempotency::begin(&idempotency_key, 24 * 60 * 60) {
            Ok(None) => {} // lock acquired
            Ok(Some(_)) => {
                // already processed
                ack_ids.push(ev.id);
                continue;
            }
            Err(_) => continue,
        }
        
        let payload: Value = serde_json::from_slice(&ev.payload).unwrap_or(Value::Null);
        
        // Example: Render localized email and dispatch
        let subject = i18n::translate("en-US", "ticket_update_subject", &[]).unwrap_or("Update".into());
        let _ = email::render("ticket_update", &[]); // placeholder for template engine
        let _ = notify::send(&notify::Message {
            channel: notify::Channel::Email,
            target: "admin@example.com".into(),
            subject,
            body: payload.to_string(),
        });
        
        // Example: Sign webhook payload and enqueue to outbox
        let _signed = webhook_sign::sign(&ev.payload, "dummy-secret", webhook_sign::Scheme::Github);
        let _ = outbox::enqueue("tenant_webhooks", &ev.payload, 0);

        // Core Event Choreography: Update Ticket Assigned
        if payload["type"].as_str() == Some("ticket_assigned") {
            if let Some(ticket_id) = payload["ticket"].as_str() {
                if let Some(assignee) = payload["assignee"].as_str() {
                    // We don't need to update the ticket here if the assignment worker 
                    // already updated it, but in strict choreography, the domain might own it.
                    // The assignment worker currently updates `records:store` directly.
                    // We will just log it here for now.
                    audit_log(&p, "choreography:ticket_assigned", ticket_id, assignee);
                }
            }
        }
        
        let _ = idempotency::complete(&idempotency_key, 200, &[]);
        ack_ids.push(ev.id);
    }
    
    if !ack_ids.is_empty() {
        let _ = eventbus::ack("helpdesk.events", "helpdesk_fanout", &ack_ids);
    }
    
    Outcome::Json(200, json!({ "processed": ack_ids.len() }).to_string())
}

// ---- inbound webhooks --------------------------------------------------------

fn ingest_email(request: &IncomingRequest) -> Outcome {
    // 1. Check signature & dedup
    // Mailgun-style example: X-Mailgun-Signature, X-Mailgun-Timestamp
    let headers = request.headers();
    let sig = get_header(&headers, "x-signature").unwrap_or_default();
    let msg_id = get_header(&headers, "message-id").unwrap_or_else(|| ids::short_code(16));
    
    let body = match read_body(request) {
        Ok(b) => b,
        Err(_) => return Outcome::Bad("could not read body".into()),
    };

    // The webhook secret key would come from configuration, assumed "mail-secret" here.
    match webhook_ingest::ingest(&body, &sig, "mail-secret", &msg_id) {
        Ok(v) => {
            if !v.accepted {
                // Duplicate delivery / replay
                return Outcome::Json(200, json!({"status": "ignored", "reason": "replay"}).to_string());
            }
        }
        Err(webhook_ingest::IngestError::BadSignature) => return Outcome::Err(401, "bad signature".into()),
        Err(webhook_ingest::IngestError::BackendUnavailable(m)) => return Outcome::Err(503, m),
    }

    // 2. Parse email
    let email = match mail_parse::parse(&body) {
        Ok(e) => e,
        Err(e) => return Outcome::Bad(format!("mail parse error: {:?}", e)),
    };

    // 3. Process to ticket/message
    ensure_seeded();

    // Check if in-reply-to matches an existing ticket (using a naive search for simplicity)
    let is_reply = email.in_reply_to.is_some();
    
    // In a real app we'd map sender to an existing user/requester. We'll use the email string.
    let requester = email.sender.clone();
    
    if is_reply {
        // Naive fallback: try to find ticket by `in_reply_to` or just create new if not found.
        // For the sake of the mock, we assume creating a new ticket if we don't have it.
        // Let's create a new ticket.
    }

    // Creating new ticket. Assignment happens asynchronously via TicketCreated event.
    let data = json!({
        "ref": format!("HD-{}", ids::short_code(6)),
        "subject": email.subject,
        "requester": requester,
        "assignee": "",
        "priority": "normal",
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
    
    let ev_payload = json!({
        "type": "ticket_created_via_email",
        "ticket": entry.id,
        "tenant": "default", // email ingester would map domain to tenant
        "requester": requester,
    });
    let _ = eventbus::publish("helpdesk.events", ev_payload.to_string().as_bytes());
    
    let msg = json!({"ticket": entry.id, "author": requester, "kind": "public", "body": email.text});
    if let Err(e) = records::create(MESSAGES, &msg.to_string(), &["ticket".to_string()]) {
        return store_err(e);
    }

    Outcome::Json(200, json!({"status": "accepted", "ticket": entry.id}).to_string())
}

fn get_header(headers: &Fields, name: &str) -> Option<String> {
    headers.get(name).first().and_then(|v| String::from_utf8(v.clone()).ok())
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
    
    let rl_key = format!("{}:ticket_create", p.tenant);
    if let Err(LimitError::Locked(secs)) = ratelimit::check(&rl_key) {
        return Outcome::Auth(AuthError::RateLimited(secs));
    }
    
    #[derive(Deserialize)]
    struct Req {
        subject: String,
        body: String,
        #[serde(default)]
        priority: Option<String>,
    }
    let req: Req = match parse(request) {
        Ok(v) => {
            // successful parse, clear rate limit
            let _ = ratelimit::reset(&rl_key);
            v
        },
        Err(m) => {
            let _ = ratelimit::record_failure(&rl_key);
            return Outcome::Bad(m);
        }
    };
    
    // quota check: 1000 tickets per month limit
    let month_secs = 30 * 24 * 60 * 60;
    if let Err(QuotaError::Exceeded(_)) = quota::reserve(&p.tenant, 1, 1000, month_secs) {
        return Outcome::Err(402, "quota exceeded".into());
    }
    
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
    
    // Creating new ticket. Assignment happens asynchronously via TicketCreated event.
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
    let ev_payload = json!({
        "type": "ticket_created",
        "ticket": entry.id,
        "tenant": p.tenant,
        "requester": p.subject,
    });
    let _ = eventbus::publish("helpdesk.events", ev_payload.to_string().as_bytes());
    let msg = json!({"ticket": entry.id, "author": p.subject, "kind": "public", "body": req.body});
    if let Err(e) = records::create(MESSAGES, &msg.to_string(), &["ticket".to_string()]) {
        return store_err(e);
    }
    
    // record usage post-hoc just to be sure, though reserve already decremented
    let _ = quota::record_usage(&p.tenant, 1, 1000, month_secs);
    
    // schedule SLA timers
    let now = wall_clock::now().seconds;
    let _ = timer::schedule_at(&format!("sla:first-response:{}", entry.id), now + 86400, &[]);
    let _ = timer::schedule_at(&format!("sla:resolution:{}", entry.id), now + 259200, &[]);
    
    // Search indexing now happens asynchronously via TicketCreated event.
    
    Outcome::Json(201, ticket_json(&entry).to_string())
}

fn search_tickets(_request: &IncomingRequest) -> Outcome {
    // In Phase 1 choreography, search indexing is decoupled.
    // Querying will move to a specialized search-domain or the Phase 3 GraphQL Gateway.
    Outcome::Err(501, "Search queries have been migrated to the GraphQL gateway.".into())
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
    let ev_payload = json!({
        "type": "message_added",
        "ticket": id,
        "tenant": p.tenant,
        "message": created.id,
        "kind": kind,
    });
    let _ = eventbus::publish("helpdesk.events", ev_payload.to_string().as_bytes());
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
        
    // check if we need to cancel SLA timers
    if agent && data["status"].as_str().unwrap_or("") == "new" {
        let _ = timer::cancel(&format!("sla:first-response:{}", id));
    }
    if status == "solved" || status == "closed" {
        let _ = timer::cancel(&format!("sla:resolution:{}", id));
        let _ = timer::cancel(&format!("sla:first-response:{}", id));
    }
        
    // Search indexing now happens asynchronously via TicketUpdated event.
    
    Outcome::Json(201, json!({"id": created.id, "kind": kind, "status": status}).to_string())
}

fn change_state(request: &IncomingRequest, id: &str) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let (entry, data) = match load_ticket(&p, id) {
        Ok(t) => t,
        Err(o) => return o,
    };
    
    let status = data["status"].as_str().unwrap_or("");
    if !is_allowed(&p, "change_state", id, status) {
        return Outcome::Forbidden("abac: forbidden to change state".into());
    }
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
            audit_log(&p, &format!("state:{}", req.event), id, "allow");
            let ev_payload = json!({
                "type": "state_changed",
                "ticket": id,
                "tenant": p.tenant,
                "status": status.state,
                "event": req.event,
            });
            let _ = eventbus::publish("helpdesk.events", ev_payload.to_string().as_bytes());
            Outcome::Json(200, json!({"status": status.state, "done": status.done}).to_string())
        }
        Err(fsm::FsmError::IllegalTransition(current)) => {
            audit_log(&p, &format!("state:{}", req.event), id, "deny");
            Outcome::Err(409, format!("cannot {} from {current}", req.event))
        }
        Err(e) => {
            audit_log(&p, &format!("state:{}", req.event), id, "error");
            Outcome::Err(503, format!("fsm: {e:?}"))
        }
    }
}

fn assign(request: &IncomingRequest, id: &str) -> Outcome {
    let p = match introspect(request) {
        Ok(p) => p,
        Err(o) => return o,
    };
    let (entry, mut data) = match load_ticket(&p, id) {
        Ok(t) => t,
        Err(o) => return o,
    };
    
    let status = data["status"].as_str().unwrap_or("");
    if !is_allowed(&p, "assign", id, status) {
        return Outcome::Forbidden("abac: forbidden to assign".into());
    }
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
        Ok(e) => {
            audit_log(&p, "assign", id, "allow");
            let ev_payload = json!({
                "type": "ticket_assigned",
                "ticket": id,
                "tenant": p.tenant,
                "assignee": req.subject,
            });
            let _ = eventbus::publish("helpdesk.events", ev_payload.to_string().as_bytes());
            Outcome::Json(200, ticket_json(&e).to_string())
        }
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

// ---- SLA Timer Breach --------------------------------------------------------

fn timers_fire(request: &IncomingRequest) -> Outcome {
    #[derive(Deserialize)]
    struct Req {
        key: String,
    }
    let req: Req = match parse(request) {
        Ok(v) => v,
        Err(m) => return Outcome::Bad(m),
    };
    
    if req.key.starts_with("sla:first-response:") || req.key.starts_with("sla:resolution:") {
        let ticket_id = req.key.split(':').nth(2).unwrap_or("");
        if ticket_id.is_empty() {
            return Outcome::Bad("invalid key".into());
        }
        
        let entry = match records::get(TICKETS, ticket_id) {
            Ok(e) => e,
            Err(_) => return Outcome::NotFound,
        };
        let mut data: Value = serde_json::from_str(&entry.data).unwrap_or(Value::Null);
        let status = data["status"].as_str().unwrap_or("");
        
        if status != "solved" && status != "closed" {
            data["priority"] = json!("urgent");
            let _ = records::update(TICKETS, ticket_id, &data.to_string(), entry.revision);
            
            let ev_payload = json!({
                "type": "sla_breached",
                "ticket": ticket_id,
                "key": req.key,
            });
            let _ = eventbus::publish("helpdesk.events", ev_payload.to_string().as_bytes());
        }
    }
    
    Outcome::Json(200, json!({"status": "ok"}).to_string())
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
/// There was no ceiling anywhere: when this was measured, 148 of the tree's 150
/// components accumulated whatever arrived until the guest hit wasmtime's 64 MiB
/// per-store memory cap and TRAPPED, which reaches the caller as a closed
/// connection saying nothing about a size.
/// A component that answers JSON has no business reading sixteen megabytes, and
/// the ones that legitimately handle uploads police it themselves with a 413 and a
/// granted max-size — those are left alone.
///
/// Generous on purpose. This is a backstop against an unbounded read, not a
/// content policy; an API that needs a real limit should state its own and say 413.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

guestio::guest_read_body!(MAX_BODY_BYTES);
guestio::guest_write_all!();

guestio::guest_bearer!();

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
        let _ = write_all(&stream, body);
    }
    let _ = OutgoingBody::finish(out, None);
}

bindings::export!(Component with_types_in bindings);
