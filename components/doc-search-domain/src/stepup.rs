//! `stepup` — the second factor, and the mark the `answer` part reads.

use crate::bindings::auth::identity::authorizer as authz;
use crate::bindings::auth::identity::types as auth_types;
use crate::bindings::otp::totp::authenticator as totp;
use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use crate::{cfg_u64, now_secs, Reply, Route};
use serde_json::{json, Value};

fn authorize(route: &Route) -> Result<auth_types::Principal, Reply> {
    if route.bearer.is_empty() {
        return Err(Reply::err(401, "unauthenticated"));
    }
    let required = auth_types::Permission { target: "docs".into(), action: "read".into() };
    match authz::authorize(&route.bearer, &required) {
        Ok(p) => Ok(p),
        Err(auth_types::AuthError::InsufficientScope(_)) => Err(Reply::err(403, "forbidden")),
        Err(auth_types::AuthError::BackendUnavailable(_))
        | Err(auth_types::AuthError::Internal(_)) => Err(Reply::err(503, "auth_unavailable")),
        Err(_) => Err(Reply::err(401, "unauthenticated")),
    }
}

/// The one lookup `answer` also does: the `stepups` row for a subject, by the
/// contract's indexed field, JSON-encoded the way `find-by` wants it.
fn find_stepup(subject: &str) -> Option<records::Entry> {
    let key = serde_json::to_string(subject).unwrap_or_default();
    records::find_by("stepups", "subject", &key).ok().and_then(|v| v.into_iter().next())
}

fn upsert_stepup(subject: &str, doc: &Value) -> Result<(), ()> {
    match find_stepup(subject) {
        Some(entry) => records::update("stepups", &entry.id, &doc.to_string(), entry.revision)
            .map(|_| ())
            .map_err(|_| ()),
        None => records::create("stepups", &doc.to_string(), &["subject".to_string()])
            .map(|_| ())
            .map_err(|_| ()),
    }
}

fn enroll(subject: &str) -> Reply {
    let provisioned = match totp::provision("docsearch", subject) {
        Ok(p) => p,
        Err(_) => return Reply::err(503, "totp_unavailable"),
    };
    // Enrolled is not verified: verified_at resets to 0, even on a re-enroll.
    let doc = json!({ "subject": subject, "verified_at": 0, "secret": provisioned.secret });
    match upsert_stepup(subject, &doc) {
        Ok(()) => Reply::json(201, json!({ "secret": provisioned.secret, "uri": provisioned.uri })),
        Err(()) => Reply::err(500, "stepup_failed"),
    }
}

fn verify(subject: &str, body: &str) -> Reply {
    let req: Value = serde_json::from_str(body).unwrap_or(json!({}));
    let code = req.get("code").and_then(Value::as_str).unwrap_or("");

    let entry = match find_stepup(subject) {
        Some(e) => e,
        None => return Reply::err(409, "not_enrolled"),
    };
    let data: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));
    let secret = match data.get("secret").and_then(Value::as_str) {
        Some(s) if !s.is_empty() => s,
        _ => return Reply::err(409, "not_enrolled"),
    };

    match totp::verify(secret, code, 30, 6, 1) {
        Ok(true) => {
            let doc = json!({ "subject": subject, "verified_at": now_secs(), "secret": secret });
            match records::update("stepups", &entry.id, &doc.to_string(), entry.revision) {
                Ok(_) => Reply::json(200, json!({ "verified": true })),
                Err(_) => Reply::err(500, "stepup_failed"),
            }
        }
        // A wrong code neither verifies nor un-verifies: `verified_at` is untouched.
        Ok(false) => Reply::err(401, "bad_code"),
        Err(_) => Reply::err(503, "totp_unavailable"),
    }
}

fn status(subject: &str) -> Reply {
    let ttl = cfg_u64("stepup-ttl-secs", 900);
    match find_stepup(subject) {
        Some(entry) => {
            let data: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));
            let verified_at = data.get("verified_at").and_then(Value::as_u64).unwrap_or(0);
            let verified = verified_at > 0 && now_secs().saturating_sub(verified_at) < ttl;
            Reply::json(200, json!({ "enrolled": true, "verified": verified }))
        }
        None => Reply::json(200, json!({ "enrolled": false, "verified": false })),
    }
}

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let principal = match authorize(route) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "mfa", "enroll"]) => enroll(&principal.subject),
        (Method::Post, ["api", "mfa", "verify"]) => verify(&principal.subject, body),
        (Method::Get, ["api", "mfa"]) => status(&principal.subject),
        _ => Reply::err(404, "not_found"),
    }
}
