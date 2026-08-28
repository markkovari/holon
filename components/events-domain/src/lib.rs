//! `events-domain` — free event ticketing, as one component.
//!
//! ## What is scaffold and what is the goal
//!
//! This file is the ROUTER and no part may write it. It dispatches to the four
//! parts, answers `/health` so a harness can tell "the component is not up" from
//! "the component is wrong", and seeds the fixture every part is judged against.
//! Four parts need it and none owns it.
//!
//! `src/events.rs`, `src/tickets.rs`, `src/checkin.rs` and `src/swaps.rs` are the
//! goal. `CONTRACT.md` is what they must agree on.
//!
//! ## Why the fixture registers people and hands back tokens
//!
//! Every route here is behind `auth:identity`, so a part cannot be judged at all
//! without a real bearer for a real principal with real roles. Making each gate
//! register its own users would put four copies of the same twenty lines in four
//! scripts, and the first one to drift would fail a part for the harness's reason
//! rather than its own. So the fixture does it once and returns the tokens.
//!
//! It does NOT create tickets. Issuing one is `tickets`' whole job, and a fixture
//! that pre-issued would let a part that never calls `quota::reserve` pass.

#[allow(warnings)]
mod bindings;
mod checkin;
mod store;
mod events;
mod swaps;
mod tickets;

use bindings::auth::identity::accounts;
use bindings::auth::identity::rbac;
use bindings::auth::identity::types::{AuthError, Permission, Principal};
use bindings::auth::identity::authorizer;
use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::records::store::store as records;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};
use serde_json::{json, Value};

guestio::guest_write_all!();

struct Component;

/// The tenant every principal in this app belongs to. One tenant: multi-tenancy is
/// `auth-guard`'s problem and it already solves it, so spending a part's budget on
/// it would test nothing about ticketing.
pub const TENANT: &str = "events";

/// What a handler answers with.
pub struct Reply {
    pub status: u16,
    /// `Value::Null` means no body at all — see `no_content`.
    pub json: Value,
}

impl Reply {
    pub fn json(status: u16, body: Value) -> Self {
        Reply { status, json: body }
    }
    pub fn err(status: u16, code: &str) -> Self {
        Reply::json(status, json!({ "error": code }))
    }
    /// 204 carries no body, and a JSON `null` is not "no body".
    pub fn no_content() -> Self {
        Reply::json(204, Value::Null)
    }
}

/// The path segments of a request, its query string, and the bearer it arrived
/// with — parsed once here so no part has to read a header.
pub struct Route {
    pub segments: Vec<String>,
    pub query: String,
    pub bearer: String,
}

impl Route {
    pub fn param(&self, key: &str) -> String {
        self.query
            .split('&')
            .filter_map(|kv| kv.split_once('='))
            .find(|(k, _)| *k == key)
            .map(|(_, v)| percent(v))
            .unwrap_or_default()
    }
}

/// `{target, action}` in one line, because every part needs several.
pub fn perm(target: &str, action: &str) -> Permission {
    Permission { target: target.to_string(), action: action.to_string() }
}

/// Authorise, or produce the reply that says why.
///
/// Shared because the mapping is a CONTRACT decision, not a per-part one: a missing
/// bearer is 401 and a valid bearer without the permission is 403, and four parts
/// deciding that separately is four chances to get it different.
pub fn require(route: &Route, target: &str, action: &str) -> Result<Principal, Reply> {
    if route.bearer.is_empty() {
        return Err(Reply::err(401, "unauthorized"));
    }
    match authorizer::authorize(&route.bearer, &perm(target, action)) {
        Ok(p) => Ok(p),
        Err(AuthError::InsufficientScope(_)) => Err(Reply::err(403, "forbidden")),
        Err(AuthError::Expired) | Err(AuthError::InvalidToken(_)) => {
            Err(Reply::err(401, "unauthorized"))
        }
        Err(_) => Err(Reply::err(401, "unauthorized")),
    }
}

pub fn has_role(p: &Principal, role: &str) -> bool {
    p.roles.iter().any(|r| r == role)
}

/// What each role may do — the app's own definition, not a fixture's.
///
/// This lived inside `seed()`, which meant the permission table was established by
/// a TEST ROUTE. With that route correctly switched off, nothing granted anything:
/// a person could register, get a token carrying `attendee`, and be refused by
/// every route in the app — including their own tickets. The gate that turned the
/// fixture off is what found it.
///
/// Idempotent, and called from both entry points a principal can arrive through,
/// because there is no startup here in which to do it once.
fn ensure_roles() {
    let roles: [(&str, &[(&str, &str)]); 3] = [
        (
            "attendee",
            &[("event", "read"), ("ticket", "read"), ("ticket", "write"), ("swap", "write")],
        ),
        (
            "organizer",
            &[
                ("event", "read"),
                ("event", "write"),
                ("ticket", "read"),
                ("ticket", "write"),
                ("checkin", "write"),
                ("swap", "write"),
            ],
        ),
        (
            "admin",
            &[
                ("event", "read"),
                ("event", "write"),
                ("ticket", "read"),
                ("ticket", "write"),
                ("checkin", "write"),
                ("swap", "write"),
            ],
        ),
    ];
    for (role, perms) in roles {
        let list: Vec<Permission> = perms.iter().map(|(t, a)| perm(t, a)).collect();
        let _ = rbac::set_role_permissions(TENANT, role, &list);
    }
}

/// Give this person the roles the DEPLOYMENT named for them.
///
/// Somebody has to be able to open the first event, and nobody can be granted a role
/// by an organizer who does not exist yet. So `organizer-emails` in the app spec says
/// who, rather than the app promoting whoever registered first — which makes the
/// account that matters depend on who got there first.
///
/// Still a grant and not a claim: the list is config, written by whoever deploys the
/// box, and a person asking for a role cannot put themselves on it.
///
/// Applied on LOGIN as well as registration, and that is the point. Doing it only at
/// registration means an operator who adds an address to the spec and redeploys
/// changes nothing for an account that already exists — the person stays an attendee
/// and there is no way in the app to fix it. Idempotent, so a login costs one
/// no-op write.
fn grant_configured_roles(email: &str, subject: &str) {
    if organizer_emails().iter().any(|e| e.eq_ignore_ascii_case(email)) {
        let _ = rbac::assign_role(TENANT, subject, "organizer");
    }
}

/// Who gets `organizer` when they register. Empty unless the deployment says so.
fn organizer_emails() -> Vec<String> {
    bindings::wasi::config::store::get("organizer-emails")
        .ok()
        .flatten()
        .unwrap_or_default()
        .split(',')
        .map(|e| e.trim().to_string())
        .filter(|e| !e.is_empty())
        .collect()
}

/// Is the fixture allowed to run at all?
///
/// It registers three people and hands their bearers back, which is exactly what a
/// gate needs and exactly what nobody else may have. It was compiled into the
/// artifact that got deployed to a real box, where the SPA called it on load — so
/// the app had no login screen because it did not need one, and neither did anyone
/// else who could reach the URL.
///
/// Absent is OFF. A switch whose entire purpose is to be off in production must not
/// depend on somebody remembering to turn it off.
fn test_routes_allowed() -> bool {
    bindings::wasi::config::store::get("allow-test-routes")
        .ok()
        .flatten()
        .is_some_and(|v| v == "1" || v == "true")
}

/// Sign up. Anyone may, and a new account is an `attendee` — the two roles that can
/// do more are granted by an admin, never claimed by the person asking.
fn register(body: &str) -> Reply {
    let Ok(input) = serde_json::from_str::<Value>(body) else {
        return Reply::err(400, "malformed_body");
    };
    let (email, password) = (
        input["email"].as_str().unwrap_or_default().trim(),
        input["password"].as_str().unwrap_or_default(),
    );
    if !email.contains('@') || password.len() < 8 {
        return Reply::err(400, "invalid");
    }
    ensure_roles();
    let principal = match accounts::register(email, password, TENANT) {
        Ok(p) => p,
        Err(AuthError::AlreadyExists) => return Reply::err(409, "already_registered"),
        Err(_) => return Reply::err(500, "register_failed"),
    };
    if rbac::assign_role(TENANT, &principal.subject, "attendee").is_err() {
        return Reply::err(500, "assign_failed");
    }
    grant_configured_roles(email, &principal.subject);
    // Log in here rather than making the caller ask twice, and AFTER the role is
    // assigned — a token minted before carries no roles and every route 403s.
    match accounts::login(email, password, TENANT) {
        Ok(t) => Reply::json(201, json!({ "token": t.access_token, "subject": principal.subject })),
        Err(_) => Reply::err(500, "login_failed"),
    }
}

fn login(body: &str) -> Reply {
    let Ok(input) = serde_json::from_str::<Value>(body) else {
        return Reply::err(400, "malformed_body");
    };
    let (email, password) = (
        input["email"].as_str().unwrap_or_default().trim(),
        input["password"].as_str().unwrap_or_default(),
    );
    ensure_roles();
    // Before the token is minted, or it carries the roles from before the grant.
    if let Ok(p) = accounts::verify_password(email, password, TENANT) {
        grant_configured_roles(email, &p.subject);
    }
    match accounts::login(email, password, TENANT) {
        Ok(t) => {
            // The SPA needs to know which screen to draw, and asking it to decode a
            // bearer to find out would put token parsing in a browser.
            let roles = accounts::verify_password(email, password, TENANT)
                .map(|p| p.roles)
                .unwrap_or_default();
            Reply::json(200, json!({ "token": t.access_token, "roles": roles }))
        }
        Err(AuthError::InvalidCredentials) => Reply::err(401, "bad_credentials"),
        Err(_) => Reply::err(401, "bad_credentials"),
    }
}

/// Three people, their roles, the permissions those roles carry, and one event —
/// everything a part needs to be judged before the other three exist.
///
/// Returns the bearers, because a gate cannot mint one.
fn seed() -> Reply {
    ensure_roles();

    // `already-exists` is not a failure here: the fixture is called by four gates
    // and by the composition gate, and it has to be idempotent or the second call
    // fails a part for the harness's reason.
    let mut tokens = json!({});
    for (email, role) in [
        ("organizer@example.test", "organizer"),
        ("attendee@example.test", "attendee"),
        ("other@example.test", "attendee"),
    ] {
        let principal = match accounts::register(email, "correct-horse", TENANT) {
            Ok(p) => p,
            Err(AuthError::AlreadyExists) => {
                match accounts::verify_password(email, "correct-horse", TENANT) {
                    Ok(p) => p,
                    Err(_) => return Reply::err(500, "seed_login_failed"),
                }
            }
            Err(_) => return Reply::err(500, "seed_register_failed"),
        };
        if rbac::assign_role(TENANT, &principal.subject, role).is_err() {
            return Reply::err(500, "seed_assign_failed");
        }
        // Log in AFTER the role is assigned, or the token carries no roles.
        let pair = match accounts::login(email, "correct-horse", TENANT) {
            Ok(t) => t,
            Err(_) => return Reply::err(500, "seed_login_failed"),
        };
        let key = email.split('@').next().unwrap_or(email);
        tokens[key] = json!({ "token": pair.access_token, "subject": principal.subject });
    }

    // TWO events, and the second one is the whole reason.
    //
    // `event_id` has capacity 3 so a gate can claim a few places without issuing a
    // hundred tickets. `contested_event_id` has capacity ONE, because a capacity
    // that is not smaller than the number of people in the fixture can never be
    // contended: three principals racing for three places all win, and the check
    // that was supposed to catch "count, compare, create" passes for a component
    // that does exactly that. One place and two claimants is the smallest shape
    // that can tell the two implementations apart.
    let organizer = tokens["organizer"]["subject"].as_str().unwrap_or_default().to_string();
    //
    // Find-or-create by title, because the fixture is called by five gates AND by
    // every pane of the screencast. Creating unconditionally made each caller add
    // two more events: the door showed the same evening four times, and a gate that
    // seeded twice was reading a different event from the one it had just claimed
    // against.
    let existing = records::find_by("events", "state", "\"open\"").unwrap_or_default();
    let mut made = Vec::new();
    for (title, capacity) in
        [("Rust, Wasm and a Free Drink", 3), ("The Last Seat In The House", 1)]
    {
        if let Some(found) = existing.iter().find(|e| {
            serde_json::from_str::<serde_json::Value>(&e.data)
                .ok()
                .and_then(|d| d["title"].as_str().map(|t| t == title))
                .unwrap_or(false)
        }) {
            made.push(found.id.clone());
            continue;
        }
        match records::create(
            "events",
            &json!({
                "title": title,
                "starts_at": "2026-09-01T18:00:00Z",
                "capacity": capacity,
                "organizer": organizer,
                "state": "open",
            })
            .to_string(),
            &["state".to_string(), "organizer".to_string()],
        ) {
            Ok(e) => made.push(e.id),
            Err(_) => return Reply::err(500, "seed_event_failed"),
        }
    }

    Reply::json(
        201,
        json!({
            "event_id": made[0],
            "contested_event_id": made[1],
            "tokens": tokens,
        }),
    )
}

/// A ceiling on a body read into memory, not a policy: past this the read gives up
/// and the body reads as empty, rather than growing until the store's memory cap
/// traps the component and the connection simply closes.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

fn read_body(request: &IncomingRequest) -> String {
    let Ok(body) = request.consume() else { return String::new() };
    let Ok(stream) = body.stream() else { return String::new() };
    let mut out = Vec::new();
    loop {
        match stream.blocking_read(64 * 1024) {
            Ok(chunk) if chunk.is_empty() => break,
            Ok(chunk) => {
                if out.len() + chunk.len() > MAX_BODY_BYTES {
                    return String::new();
                }
                out.extend_from_slice(&chunk);
            }
            Err(bindings::wasi::io::streams::StreamError::Closed) => break,
            // No error channel here, so the choice is a truncated body or none.
            // None: a caller parsing an empty body fails cleanly, where half a JSON
            // document can parse into something plausible and wrong.
            Err(_) => return String::new(),
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn percent(s: &str) -> String {
    let b = s.replace('+', " ");
    let b = b.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match (b[i], b.get(i + 1), b.get(i + 2)) {
            (b'%', Some(h), Some(l)) => {
                match u8::from_str_radix(core::str::from_utf8(&[*h, *l]).unwrap_or("zz"), 16) {
                    Ok(v) => {
                        out.push(v);
                        i += 3;
                    }
                    Err(_) => {
                        out.push(b[i]);
                        i += 1;
                    }
                }
            }
            _ => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let path = request.path_with_query().unwrap_or_else(|| "/".into());
        let (raw_path, query) = match path.split_once('?') {
            Some((p, q)) => (p.to_string(), q.to_string()),
            None => (path.clone(), String::new()),
        };
        let bearer = request
            .headers()
            .get("authorization")
            .first()
            .map(|v| String::from_utf8_lossy(v).into_owned())
            .unwrap_or_default()
            .trim_start_matches("Bearer ")
            .to_string();
        let route = Route {
            segments: raw_path.split('/').filter(|s| !s.is_empty()).map(percent).collect(),
            query,
            bearer,
        };
        let method = request.method();
        let body = match method {
            Method::Post | Method::Put | Method::Patch | Method::Delete => read_body(&request),
            _ => String::new(),
        };

        let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
        let Reply { status, json: payload } = match seg.as_slice() {
            ["health"] => Reply::json(200, json!({ "ok": true })),
            ["api", "register"] => register(&body),
            ["api", "login"] => login(&body),
            // 404 rather than 403 when it is off: a route that is not there should
            // not advertise that it exists somewhere else.
            ["test", ..] if !test_routes_allowed() => Reply::err(404, "not_found"),
            ["test", "seed"] => seed(),
            // Scaffold, and it says what it is: a part must be judgeable on what it
            // WROTE without depending on the part that owns the read route. The
            // check-in gate needs to see a ticket document while `tickets` is still
            // a stub answering `not_implemented`.
            ["test", coll @ ("events" | "tickets" | "swaps"), id] => {
                match records::get(coll, id) {
                    Ok(e) => Reply::json(200, serde_json::from_str(&e.data).unwrap_or(json!({}))),
                    Err(_) => Reply::err(404, "not_found"),
                }
            }
            // Before the events arm: a ticket claim is nested under an event, and a
            // match on ["api","events",..] would hand it to `events` instead.
            ["api", "events", _, "tickets"] => tickets::handle(&method, &route, &body),
            ["api", "events", ..] => events::handle(&method, &route, &body),
            ["api", "tickets", ..] => tickets::handle(&method, &route, &body),
            ["api", "checkin"] => checkin::handle(&method, &route, &body),
            ["api", "swaps", ..] => swaps::handle(&method, &route, &body),
            _ => Reply::err(404, "not_found"),
        };

        let headers = Fields::new();
        let _ = headers.set("content-type", &[b"application/json".to_vec()]);
        let resp = OutgoingResponse::new(headers);
        let _ = resp.set_status_code(status);
        let out = resp.body().expect("body");
        ResponseOutparam::set(response_out, Ok(resp));
        if let Ok(stream) = out.write() {
            if !payload.is_null() {
                let _ = write_all(&stream, payload.to_string().as_bytes());
            }
            drop(stream);
        }
        let _ = OutgoingBody::finish(out, None);
    }
}

bindings::export!(Component with_types_in bindings);
