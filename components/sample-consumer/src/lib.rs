//! Sample HTTP consumer. Proves the auth:identity contract: every request is
//! guarded by a single `authorizer.authorize(token, required)` call.
//!
//! - No `Authorization: Bearer <token>` header           -> 401
//! - Token valid but lacks the required permission        -> 403
//! - Token valid and authorized                           -> 200 + principal
//!
//! The required permission for this demo endpoint is { target: "demo",
//! action: "read" }.

#[allow(warnings)]
mod bindings;

use bindings::auth::identity::authorizer::{authorize, Permission};
use bindings::auth::identity::types::{AuthError, Principal};
use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

struct Component;

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let status = match guard(&request) {
            Ok(principal) => {
                let body = format!(
                    "{{\"ok\":true,\"subject\":\"{}\",\"tenant\":\"{}\",\"roles\":{:?}}}",
                    principal.subject, principal.tenant, principal.roles
                );
                respond(response_out, 200, &body);
                return;
            }
            Err(e) => e,
        };
        let (code, msg) = error_response(&status);
        respond(response_out, code, &format!("{{\"ok\":false,\"error\":\"{msg}\"}}"));
    }
}

fn guard(request: &IncomingRequest) -> Result<Principal, AuthError> {
    let token = bearer_token(request)
        .ok_or_else(|| AuthError::InvalidToken("missing bearer token".into()))?;
    let required = Permission {
        target: "demo".to_string(),
        action: "read".to_string(),
    };
    authorize(&token, &required)
}

/// Pull the bearer token out of the `Authorization` header.
fn bearer_token(request: &IncomingRequest) -> Option<String> {
    let headers = request.headers();
    let values = headers.get(&"authorization".to_string());
    for v in values {
        if let Ok(s) = String::from_utf8(v) {
            if let Some(tok) = s.strip_prefix("Bearer ") {
                return Some(tok.trim().to_string());
            }
        }
    }
    None
}

/// Map an auth-error to (HTTP status, message).
fn error_response(e: &AuthError) -> (u16, &'static str) {
    match e {
        AuthError::InsufficientScope(_) => (403, "insufficient_scope"),
        AuthError::Expired => (401, "expired"),
        AuthError::InvalidToken(_) => (401, "invalid_token"),
        AuthError::InvalidCredentials => (401, "invalid_credentials"),
        AuthError::AlreadyExists => (409, "already_exists"),
        AuthError::RateLimited => (429, "rate_limited"),
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
    let out_body = response.body().expect("outgoing body");
    ResponseOutparam::set(response_out, Ok(response));
    let stream = out_body.write().expect("write stream");
    stream
        .blocking_write_and_flush(body.as_bytes())
        .expect("write body");
    drop(stream);
    OutgoingBody::finish(out_body, None).expect("finish body");
}

bindings::export!(Component with_types_in bindings);
