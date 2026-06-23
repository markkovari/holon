//! HTTP JSON register/login app. Owns NO auth logic — it maps HTTP onto the
//! auth:identity contract:
//!
//!   POST /register {email,password,tenant?}  -> 201 {subject,tenant}
//!   POST /login    {email,password,tenant?}  -> 200 {access_token,refresh_token,...}
//!   GET  /me       (Authorization: Bearer)   -> 200 {subject,tenant,roles}
//!   POST /logout   (Authorization: Bearer)   -> 204
//!
//! /me and /logout use the session token returned by /login.

#[allow(warnings)]
mod bindings;

use serde::Deserialize;

use bindings::auth::identity::accounts;
use bindings::auth::identity::authorizer;
use bindings::auth::identity::rbac;
use bindings::auth::identity::session;
use bindings::auth::identity::types::{AuthError, Permission, Principal, TokenPair};
use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

struct Component;

#[derive(Deserialize)]
struct Credentials {
    email: String,
    password: String,
    #[serde(default)]
    tenant: Option<String>,
}

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let method = request.method();
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let route = path.split('?').next().unwrap_or("/");

        let result = match (&method, route) {
            (Method::Post, "/register") => register(&request),
            (Method::Post, "/login") => login(&request),
            (Method::Get, "/me") => me(&request),
            (Method::Post, "/logout") => logout(&request),
            (Method::Post, "/verify") => verify(&request),
            (Method::Post, "/admin/role-permissions") => admin_set_role_perms(&request),
            (Method::Post, "/admin/assign-role") => admin_assign_role(&request),
            _ => Outcome::NotFound,
        };

        match result {
            Outcome::Json(code, body) => respond(response_out, code, &body),
            Outcome::Empty(code) => respond(response_out, code, ""),
            Outcome::Err(e) => {
                let (code, msg) = error_response(&e);
                respond(response_out, code, &format!("{{\"error\":\"{msg}\"}}"));
            }
            Outcome::NotFound => respond(response_out, 404, "{\"error\":\"not_found\"}"),
            Outcome::Bad(msg) => {
                respond(response_out, 400, &format!("{{\"error\":\"{msg}\"}}"))
            }
        }
    }
}

enum Outcome {
    Json(u16, String),
    Empty(u16),
    Err(AuthError),
    Bad(String),
    NotFound,
}

fn register(request: &IncomingRequest) -> Outcome {
    let creds = match parse_credentials(request) {
        Ok(c) => c,
        Err(msg) => return Outcome::Bad(msg),
    };
    let tenant = creds.tenant.unwrap_or_default();
    match accounts::register(&creds.email, &creds.password, &tenant) {
        Ok(p) => Outcome::Json(201, principal_json(&p)),
        Err(e) => Outcome::Err(e),
    }
}

fn login(request: &IncomingRequest) -> Outcome {
    let creds = match parse_credentials(request) {
        Ok(c) => c,
        Err(msg) => return Outcome::Bad(msg),
    };
    let tenant = creds.tenant.unwrap_or_default();
    match accounts::login(&creds.email, &creds.password, &tenant) {
        Ok(tp) => Outcome::Json(200, token_pair_json(&tp)),
        Err(e) => Outcome::Err(e),
    }
}

fn me(request: &IncomingRequest) -> Outcome {
    let token = match bearer_token(request) {
        Some(t) => t,
        None => return Outcome::Err(AuthError::InvalidToken("missing bearer".into())),
    };
    // introspect = verify with no permission requirement.
    match authorizer::introspect(&token) {
        Ok(p) => Outcome::Json(200, principal_json(&p)),
        Err(e) => Outcome::Err(e),
    }
}

fn logout(request: &IncomingRequest) -> Outcome {
    let token = match bearer_token(request) {
        Some(t) => t,
        None => return Outcome::Err(AuthError::InvalidToken("missing bearer".into())),
    };
    match session::revoke(&token) {
        Ok(()) => Outcome::Empty(204),
        Err(e) => Outcome::Err(e),
    }
}

#[derive(Deserialize)]
struct VerifyReq {
    target: String,
    action: String,
}

/// Generic guard endpoint for downstream apps: verify the bearer token AND
/// require permission {target, action}. 200 + principal if allowed, 403 if
/// authenticated-but-unauthorized, 401 on bad/expired token.
fn verify(request: &IncomingRequest) -> Outcome {
    let token = match bearer_token(request) {
        Some(t) => t,
        None => return Outcome::Err(AuthError::InvalidToken("missing bearer".into())),
    };
    let body = match read_body(request) {
        Ok(b) => b,
        Err(_) => return Outcome::Bad("could not read body".into()),
    };
    let req: VerifyReq = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return Outcome::Bad(format!("bad json: {e}")),
    };
    let perm = Permission { target: req.target, action: req.action };
    match authorizer::authorize(&token, &perm) {
        Ok(p) => Outcome::Json(200, principal_json(&p)),
        Err(e) => Outcome::Err(e),
    }
}

// ---- admin (RBAC seeding) ------------------------------------------------
//
// NOTE: these routes are UNAUTHENTICATED in this example for simplicity. In a
// real deployment, guard them (e.g. `authorizer.authorize(token, {target:
// "rbac", action: "admin"})`) or expose them only on an internal network.

#[derive(Deserialize)]
struct PermDto {
    target: String,
    action: String,
}

#[derive(Deserialize)]
struct SetRolePermsReq {
    tenant: String,
    role: String,
    permissions: Vec<PermDto>,
}

#[derive(Deserialize)]
struct AssignRoleReq {
    tenant: String,
    subject: String,
    role: String,
}

/// POST /admin/role-permissions — define what a role grants. 204 on success.
fn admin_set_role_perms(request: &IncomingRequest) -> Outcome {
    let body = match read_body(request) {
        Ok(b) => b,
        Err(_) => return Outcome::Bad("could not read body".into()),
    };
    let req: SetRolePermsReq = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return Outcome::Bad(format!("bad json: {e}")),
    };
    let perms: Vec<Permission> = req
        .permissions
        .into_iter()
        .map(|p| Permission { target: p.target, action: p.action })
        .collect();
    match rbac::set_role_permissions(&req.tenant, &req.role, &perms) {
        Ok(()) => Outcome::Empty(204),
        Err(e) => Outcome::Err(e),
    }
}

/// POST /admin/assign-role — grant a role to a subject. 204 on success.
fn admin_assign_role(request: &IncomingRequest) -> Outcome {
    let body = match read_body(request) {
        Ok(b) => b,
        Err(_) => return Outcome::Bad("could not read body".into()),
    };
    let req: AssignRoleReq = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return Outcome::Bad(format!("bad json: {e}")),
    };
    match rbac::assign_role(&req.tenant, &req.subject, &req.role) {
        Ok(()) => Outcome::Empty(204),
        Err(e) => Outcome::Err(e),
    }
}

// ---- request helpers ----------------------------------------------------

fn parse_credentials(request: &IncomingRequest) -> Result<Credentials, String> {
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

fn bearer_token(request: &IncomingRequest) -> Option<String> {
    let headers = request.headers();
    for v in headers.get(&"authorization".to_string()) {
        if let Ok(s) = String::from_utf8(v) {
            if let Some(tok) = s.strip_prefix("Bearer ") {
                return Some(tok.trim().to_string());
            }
        }
    }
    None
}

// ---- responses ----------------------------------------------------------

fn principal_json(p: &Principal) -> String {
    format!(
        "{{\"subject\":\"{}\",\"tenant\":\"{}\",\"roles\":{},\"scopes\":{}}}",
        p.subject,
        p.tenant,
        json_str_array(&p.roles),
        json_str_array(&p.scopes),
    )
}

fn token_pair_json(tp: &TokenPair) -> String {
    let refresh = match &tp.refresh_token {
        Some(r) => format!("\"{r}\""),
        None => "null".to_string(),
    };
    let session = match &tp.session_id {
        Some(s) => format!("\"{s}\""),
        None => "null".to_string(),
    };
    format!(
        "{{\"access_token\":\"{}\",\"refresh_token\":{},\"expires_in\":{},\"session_id\":{}}}",
        tp.access_token, refresh, tp.expires_in, session
    )
}

fn json_str_array(items: &[String]) -> String {
    let inner: Vec<String> = items.iter().map(|s| format!("\"{s}\"")).collect();
    format!("[{}]", inner.join(","))
}

fn error_response(e: &AuthError) -> (u16, &'static str) {
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

fn respond(response_out: ResponseOutparam, status: u16, body: &str) {
    let headers = Fields::new();
    let _ = headers.set(&"content-type".to_string(), &[b"application/json".to_vec()]);
    let response = OutgoingResponse::new(headers);
    let _ = response.set_status_code(status);
    let out = response.body().expect("outgoing body");
    ResponseOutparam::set(response_out, Ok(response));
    if !body.is_empty() {
        let stream = out.write().expect("write stream");
        let _ = stream.blocking_write_and_flush(body.as_bytes());
    }
    let _ = OutgoingBody::finish(out, None);
}

bindings::export!(Component with_types_in bindings);
