//! `crm-domain` — router: static SPA, auth (register/login/logout/me), and
//! dispatch into `handlers.rs` for the domain routes. Mirrors
//! `ticket-triage-domain`/`track-domain`'s own split: nothing domain-specific
//! lives here.

#[allow(warnings)]
mod bindings;
mod handlers;

use bindings::auth::identity::accounts;
use bindings::auth::identity::authorizer;
use bindings::auth::identity::rbac;
use bindings::auth::identity::session;
use bindings::auth::identity::types::{AuthError, Principal};
use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};
use serde::Deserialize;
use serde_json::{json, Value};

pub const TENANT: &str = "timesheet";
/// The only roles this app grants. Anything else in a register request falls
/// back to `rep` — a role is a privilege, not a free-text field.
const ROLES: &[&str] = &["manager", "member"];

guestio::guest_write_all!();
guestio::guest_bearer!();

const MAX_BODY_BYTES: usize = 1024 * 1024;
guestio::guest_read_body_text!(MAX_BODY_BYTES);

struct Component;

pub struct Reply {
    pub status: u16,
    pub json: Value,
}

impl Reply {
    pub fn json(status: u16, body: Value) -> Self {
        Reply { status, json: body }
    }
    pub fn err(status: u16, code: &str) -> Self {
        Reply::json(status, json!({ "error": code }))
    }
    pub fn auth_err(e: AuthError) -> Self {
        match e {
            AuthError::InsufficientScope(_) => Reply::err(403, "forbidden"),
            AuthError::InvalidCredentials => Reply::err(401, "invalid_credentials"),
            AuthError::AlreadyExists => Reply::err(409, "already_exists"),
            AuthError::Malformed(_) => Reply::err(400, "malformed"),
            AuthError::RateLimited(_) => Reply::err(429, "rate_limited"),
            _ => Reply::err(401, "unauthorized"),
        }
    }
}

pub struct Route {
    pub segments: Vec<String>,
    pub bearer: String,
}

/// A verified caller for a domain route. `handlers.rs` reads `roles` directly
/// (the same style `track-domain` uses) rather than going through
/// `auth:identity/rbac`'s permission-mapping layer — this app has exactly two
/// roles, which a role check says as plainly as a permission check would,
/// with no `rbac::set-role-permissions` seeding step to keep in sync.
pub fn introspect(route: &Route) -> Result<Principal, Reply> {
    if route.bearer.is_empty() {
        return Err(Reply::err(401, "unauthorized"));
    }
    authorizer::introspect(&route.bearer).map_err(Reply::auth_err)
}

pub fn is_manager(p: &Principal) -> bool {
    p.roles.iter().any(|r| r == "manager")
}

pub fn audit(event: &str, outcome: &str, subject: &str, detail: &str) {
    use bindings::audit::log::recorder as audit_rec;
    use bindings::audit::log::types::Event;
    let _ = audit_rec::record_event(&Event {
        id: String::new(),
        trace_id: String::new(),
        span_id: String::new(),
        timestamp: now_secs(),
        event: event.to_string(),
        outcome: outcome.to_string(),
        tenant: TENANT.to_string(),
        subject: subject.to_string(),
        detail: detail.to_string(),
    });
}

pub fn now_secs() -> u64 {
    bindings::wasi::clocks::wall_clock::now().seconds
}

#[derive(Deserialize)]
struct RegisterReq {
    email: String,
    password: String,
    #[serde(default)]
    role: Option<String>,
}

fn register(body: &str) -> Reply {
    let req: RegisterReq = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return Reply::err(400, "bad_json"),
    };
    let principal = match accounts::register(&req.email, &req.password, TENANT) {
        Ok(p) => p,
        Err(e) => return Reply::auth_err(e),
    };
    let wanted = req.role.unwrap_or_else(|| "member".to_string());
    let role = if ROLES.contains(&wanted.as_str()) { wanted } else { "member".to_string() };
    let _ = rbac::assign_role(&principal.tenant, &principal.subject, &role);
    audit("account.register", "allow", &principal.subject, &role);
    Reply::json(201, json!({"subject": principal.subject, "role": role}))
}

#[derive(Deserialize)]
struct LoginReq {
    email: String,
    password: String,
}

fn login(body: &str) -> Reply {
    let req: LoginReq = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(_) => return Reply::err(400, "bad_json"),
    };
    match accounts::login(&req.email, &req.password, TENANT) {
        Ok(tp) => {
            audit("account.login", "allow", &req.email, "");
            Reply::json(
                200,
                json!({
                    "access_token": tp.access_token,
                    "refresh_token": tp.refresh_token,
                    "expires_in": tp.expires_in,
                }),
            )
        }
        Err(e) => {
            audit("account.login", "deny", &req.email, "");
            Reply::auth_err(e)
        }
    }
}

fn logout(route: &Route) -> Reply {
    if route.bearer.is_empty() {
        return Reply::err(401, "unauthorized");
    }
    match session::revoke(&route.bearer) {
        Ok(()) => Reply::json(204, Value::Null),
        Err(e) => Reply::auth_err(e),
    }
}

fn me(route: &Route) -> Reply {
    match introspect(route) {
        Ok(p) => Reply::json(200, json!({"subject": p.subject, "tenant": p.tenant, "roles": p.roles})),
        Err(r) => r,
    }
}

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let path = request.path_with_query().unwrap_or_else(|| "/".into());
        let raw_path = path.split('?').next().unwrap_or("/").to_string();
        let bearer = bearer(&request).unwrap_or_default();
        let method = request.method();
        let body = match method {
            Method::Post | Method::Put | Method::Patch => read_body(&request),
            _ => String::new(),
        };
        let segments: Vec<String> =
            raw_path.split('/').filter(|s| !s.is_empty()).map(str::to_string).collect();
        let route = Route { segments, bearer };
        let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();

        if seg.as_slice() == ["health"] {
            return emit(response_out, Reply::json(200, json!({"ok": true})));
        }

        let reply = match (&method, seg.as_slice()) {
            (Method::Post, ["register"]) => register(&body),
            (Method::Post, ["login"]) => login(&body),
            (Method::Post, ["logout"]) => logout(&route),
            (Method::Get, ["me"]) => me(&route),
            (_, ["api", ..]) => handlers::handle(&method, &route, &body),
            _ => Reply::err(404, "not_found"),
        };
        emit(response_out, reply);
    }
}

fn emit(response_out: ResponseOutparam, reply: Reply) {
    let headers = Fields::new();
    let _ = headers.set("content-type", &[b"application/json".to_vec()]);
    let resp = OutgoingResponse::new(headers);
    let _ = resp.set_status_code(reply.status);
    let out = resp.body().expect("body");
    ResponseOutparam::set(response_out, Ok(resp));
    if let Ok(stream) = out.write() {
        if !reply.json.is_null() {
            let _ = write_all(&stream, reply.json.to_string().as_bytes());
        }
        drop(stream);
    }
    let _ = OutgoingBody::finish(out, None);
}

bindings::export!(Component with_types_in bindings);
