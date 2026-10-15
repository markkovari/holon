//! `events-domain` — free event ticketing, as one component.
//!
//! This file is the ROUTER and no part may write it: it dispatches to the four parts, answers
//! `/health`, and seeds the fixture every part is judged against. `src/events.rs`,
//! `src/tickets.rs`, `src/checkin.rs` and `src/swaps.rs` are the goal; `CONTRACT.md` is what
//! they must agree on.
//!
//! The fixture registers people and hands back tokens: every route is behind
//! `auth:identity`, and per-gate registration would put four copies of the same twenty lines
//! in four scripts. It does NOT create tickets: issuing one is `tickets`' whole job, and
//! pre-issuing would let a part that never calls `quota::reserve` pass.

#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/p3",
            "../../wit/deps/wasi-keyvalue-0.2.0-draft",
            "../../wit/deps/wasi-random-0.2.0",
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../../host/wit/deps/comp-store",
            "../record-store/wit",
            "../id-generate/wit",
            "../quota/wit",
            "../qr/wit",
            "../fsm-workflow/wit",
            "../../wit/deps/wasi-config-0.2.0-rc.1",
            "../../wit/deps/ratelimit-guard",
            "../../wit/deps/wasi-cli-0.2.0",
            "../../wit/deps/wasi-http-0.2.0",
            "../audit-log/wit",
            "../../wit/auth.wit",
            "../../wit/deps/wasi-blobstore-0.2.0-draft",
            "../../wit/deps/wasmcloud-messaging-0.2.0",
            "../../host/wit/deps/comp-secrets",
            "../notify-inbox/wit",
            "../mail-http/wit",
            "../notify-prefs/wit",
            "../webhook-sign/wit",
            "../scheduler-timer/wit",
            "../blob-store/wit",
            "../upload-policy/wit",
            "wit",
        ],
        world: "events:ticketing/events-domain",
        generate_all,
    });
    /// Stable names for the p3 WASI modules (see `guestio`'s p3 section).
    pub mod p3 {
        pub use super::exports::wasi::http0_3_0_rc_2026_03_15::handler;
        pub use super::wasi::clocks0_3_0_rc_2026_03_15 as clocks;
        pub use super::wasi::http0_3_0_rc_2026_03_15 as http;
    }
}
mod checkin;
mod events;
mod notifications;
mod remind;
mod store;
mod swaps;
mod tickets;

use bindings::auth::identity::accounts;
use bindings::auth::identity::authorizer;
use bindings::auth::identity::rbac;
use bindings::auth::identity::types::{AuthError, Permission, Principal};
use bindings::p3::handler::Guest;
use bindings::p3::http::types::{ErrorCode, Method, Request, Response};
use bindings::records::store::store as records;
use serde_json::{json, Value};

guestio::guest_p3_respond!();

struct Component;

/// The tenant every principal belongs to. One tenant: multi-tenancy is `auth-guard`'s problem.
pub const TENANT: &str = "events";

pub struct Reply {
    pub status: u16,
    /// `Value::Null` means no body at all — see `no_content`.
    pub json: Value,
    /// A body the router must NOT serialise as JSON, with its content type: `Value::String`
    /// serialises to a JSON string LITERAL, so a browser asked to render an image gets a quoted
    /// blob.
    pub raw: Option<(String, Vec<u8>)>,
}

impl Reply {
    pub fn json(status: u16, body: Value) -> Self {
        Reply { status, json: body, raw: None }
    }
    /// Bytes through, byte for byte, under the content type you name.
    pub fn raw(status: u16, content_type: &str, bytes: Vec<u8>) -> Self {
        Reply { status, json: Value::Null, raw: Some((content_type.to_string(), bytes)) }
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

/// Authorise, or produce the reply that says why. Shared because the mapping is a CONTRACT
/// decision: a missing bearer is 401, a valid bearer without the permission is 403.
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

/// What each role may do. Lives here, not in `seed()`: with the test route off, nothing else
/// would grant anything. Idempotent, called from both entry points a principal can arrive
/// through (there is no startup).
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

/// Give this person the roles the DEPLOYMENT named for them: `organizer-emails` in the app
/// spec says who (somebody must be able to open the first event), rather than promoting
/// whoever registered first. Still a grant, not a claim: the list is config. Applied on LOGIN
/// as well as registration, so an operator who adds an address and redeploys changes an
/// existing account too. Idempotent.
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

/// Is the fixture allowed to run at all? It registers three people and hands their bearers
/// back, which only a gate may have: it once shipped to a real box where the SPA called it
/// on load. Absent is OFF; a switch whose purpose is to be off in production must not depend
/// on somebody remembering to turn it off.
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

/// Three people, their roles, the permissions those roles carry, and one event, so a part can
/// be judged before the other three exist. Returns the bearers, because a gate cannot mint one.
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

    // TWO events, and the second is the point: `contested_event_id` has capacity ONE, because
    // with capacity not smaller than the fixture's people nothing can be contended (three
    // principals racing for three places all win, so "count, compare, create" would pass).
    // One place and two claimants is the smallest shape that tells the implementations apart.
    let organizer = tokens["organizer"]["subject"].as_str().unwrap_or_default().to_string();
    // Find-or-create by title: the fixture is called by five gates AND every screencast pane,
    // and creating unconditionally added two events per caller.
    let existing = records::find_by("events", "state", "\"open\"").unwrap_or_default();
    let mut made = Vec::new();
    for (title, capacity) in [("Rust, Wasm and a Free Drink", 3), ("The Last Seat In The House", 1)]
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

// Body as BYTES vs text is a real choice: `read_body` decodes lossily (right for JSON),
// which destroys an image (non-UTF-8 becomes U+FFFD, so the upload stores a non-JPEG).
// Binary goes through `read_body_bytes`.
guestio::guest_p3_read_body_text!(MAX_BODY_BYTES);

use guestfmt::percent_decode as percent;

impl Guest for Component {
    async fn handle(request: Request) -> Result<Response, ErrorCode> {
        let path = request.get_path_with_query().unwrap_or_else(|| "/".into());
        let (raw_path, query) = match path.split_once('?') {
            Some((p, q)) => (p.to_string(), q.to_string()),
            None => (path.clone(), String::new()),
        };
        let bearer = request
            .get_headers()
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
        let method = request.get_method();
        // The content type the caller sent, needed before the body is consumed —
        // `request.get_headers()` is not readable once the body has been consumed.
        let content_type = request
            .get_headers()
            .get("content-type")
            .first()
            .map(|v| String::from_utf8_lossy(v).into_owned())
            .unwrap_or_default();
        let is_image_upload = matches!(method, Method::Post | Method::Put)
            && route.segments.len() == 4
            && route.segments[0] == "api"
            && route.segments[1] == "events"
            && route.segments[3] == "image";
        // A body is read ONCE — the stream is not rewindable — so which shape it is
        // read into has to be decided before reading, not after.
        let (body, bytes) = match method {
            _ if is_image_upload => {
                (String::new(), read_body_bytes(request).await.unwrap_or_default())
            }
            Method::Post | Method::Put | Method::Patch | Method::Delete => {
                (read_body(request).await, Vec::new())
            }
            _ => (String::new(), Vec::new()),
        };

        let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();

        // The stream never returns a `Reply`. It sets the response itself and then
        // writes to it for as long as the connection lasts, so it has to be taken
        // out of the dispatch table above the thing that expects a body back.
        if seg.as_slice() == ["api", "notifications", "stream"] {
            return notifications::stream(&route);
        }

        let Reply { status, json: payload, raw } = match seg.as_slice() {
            ["health"] => Reply::json(200, json!({ "ok": true })),
            ["api", "register"] => register(&body),
            ["api", "login"] => login(&body),
            // 404 rather than 403 when it is off: a route that is not there should
            // not advertise that it exists somewhere else.
            ["test", ..] if !test_routes_allowed() => Reply::err(404, "not_found"),
            ["test", "seed"] => seed(),
            // Scaffold: a part must be judgeable on what it WROTE without the part that owns
            // the read route (the check-in gate needs a ticket document while `tickets` is a stub).
            ["test", coll @ ("events" | "tickets" | "swaps"), id] => match records::get(coll, id) {
                Ok(e) => Reply::json(200, serde_json::from_str(&e.data).unwrap_or(json!({}))),
                Err(_) => Reply::err(404, "not_found"),
            },
            // Before the events arm: a ticket claim is nested under an event, and a
            // match on ["api","events",..] would hand it to `events` instead.
            ["api", "reminders", "run"] => remind::run(&method, &route),
            ["api", "events", id, "reminder"] => remind::peek(&route, id),
            ["api", "notifications", ..] | ["api", "prefs"] => {
                notifications::handle(&method, &route, &body)
            }
            ["api", "events", id, "image"] => {
                events::image(&method, &route, id, &content_type, bytes)
            }
            ["api", "events", _, "tickets"] => tickets::handle(&method, &route, &body),
            ["api", "events", ..] => events::handle(&method, &route, &body),
            ["api", "tickets", ..] => tickets::handle(&method, &route, &body),
            ["api", "checkin"] => checkin::handle(&method, &route, &body),
            ["api", "swaps", ..] => swaps::handle(&method, &route, &body),
            _ => Reply::err(404, "not_found"),
        };

        match raw {
            Some((ct, b)) => respond(status, &ct, b),
            None if !payload.is_null() => respond(status, "application/json", payload.to_string()),
            None => respond(status, "application/json", Vec::new()),
        }
    }
}

bindings::export!(Component with_types_in bindings);
