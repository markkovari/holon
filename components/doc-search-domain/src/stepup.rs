use crate::{cfg_u64, now_secs, Reply, Route};
use crate::bindings::auth::identity::authorizer as authz;
use crate::bindings::auth::identity::types::Permission;
use crate::bindings::otp::totp::authenticator as totp;
use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use serde_json::{json, Value};

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "mfa", "enroll"]) => enroll(route),
        (Method::Post, ["api", "mfa", "verify"]) => verify(route, body),
        (Method::Get, ["api", "mfa"]) => status(route),
        _ => Reply::err(404, "not_found"),
    }
}

fn authorize_perm(route: &Route, action: &str) -> Result<String, Reply> {
    let perm = Permission { target: "docs".to_string(), action: action.to_string() };
    match authz::authorize(&route.bearer, &perm) {
        Ok(p) => Ok(p.subject),
        Err(err) => {
            use crate::bindings::auth::identity::types::AuthError;
            let reply = match err {
                AuthError::InsufficientScope(_) => Reply::err(403, "forbidden"),
                AuthError::BackendUnavailable(_) | AuthError::Internal(_) => Reply::err(503, "auth_unavailable"),
                _ => Reply::err(401, "unauthenticated"),
            };
            Err(reply)
        }
    }
}

fn enroll(route: &Route) -> Reply {
    let subject = match authorize_perm(route, "read") {
        Ok(s) => s,
        Err(r) => return r,
    };
    
    let prov = match totp::provision("docsearch", &subject) {
        Ok(p) => p,
        Err(_) => return Reply::err(500, "provision_failed"),
    };

    let doc = json!({
        "subject": subject,
        "verified_at": 0,
        "secret": prov.secret
    });
    
    let entries = records::find_by("stepups", "subject", &json!(subject).to_string()).unwrap_or_default();
    if let Some(existing) = entries.first() {
        if records::update("stepups", &existing.id, &doc.to_string(), existing.revision).is_err() {
            return Reply::err(500, "store_error");
        }
    } else {
        if records::create("stepups", &doc.to_string(), &["subject".to_string()]).is_err() {
            return Reply::err(500, "store_error");
        }
    }

    Reply::json(201, json!({ "secret": prov.secret, "uri": prov.uri }))
}

fn verify(route: &Route, body: &str) -> Reply {
    let subject = match authorize_perm(route, "read") {
        Ok(s) => s,
        Err(r) => return r,
    };
    
    let req: Value = serde_json::from_str(body).unwrap_or(json!({}));
    let code = req.get("code").and_then(Value::as_str).unwrap_or("");

    let entries = records::find_by("stepups", "subject", &json!(subject).to_string()).unwrap_or_default();
    let entry = match entries.first() {
        Some(e) => e,
        None => return Reply::err(409, "not_enrolled"),
    };
    
    let doc: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));
    let secret = doc.get("secret").and_then(Value::as_str).unwrap_or("");

    match totp::verify(secret, code, 30, 6, 1) {
        Ok(true) => {
            let mut updated = doc.clone();
            updated["verified_at"] = json!(now_secs());
            if records::update("stepups", &entry.id, &updated.to_string(), entry.revision).is_err() {
                return Reply::err(500, "store_error");
            }
            Reply::json(200, json!({ "verified": true }))
        }
        Ok(false) => Reply::err(401, "bad_code"),
        Err(_) => Reply::err(503, "totp_unavailable"),
    }
}

fn status(route: &Route) -> Reply {
    let subject = match authorize_perm(route, "read") {
        Ok(s) => s,
        Err(r) => return r,
    };
    
    let entries = records::find_by("stepups", "subject", &json!(subject).to_string()).unwrap_or_default();
    if let Some(entry) = entries.first() {
        let doc: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));
        let verified_at = doc.get("verified_at").and_then(Value::as_u64).unwrap_or(0);
        let ttl = cfg_u64("stepup-ttl-secs", 900);
        let verified = verified_at > 0 && (now_secs() - verified_at) <= ttl;
        Reply::json(200, json!({ "enrolled": true, "verified": verified }))
    } else {
        Reply::json(200, json!({ "enrolled": false, "verified": false }))
    }
}
