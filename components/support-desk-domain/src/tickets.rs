use crate::bindings::auth::identity::authorizer as authz;
use crate::bindings::auth::identity::types::Permission;
use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use crate::{cfg_u64, now_secs, Reply, Route};
use serde_json::{json, Value};

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "tickets"]) => create_ticket(route, body),
        (Method::Get, ["api", "tickets", id]) => get_ticket(route, id),
        (Method::Get, ["api", "tickets"]) => list_tickets(route),
        _ => Reply::err(404, "not_found"),
    }
}

fn authorize_perm(route: &Route, action: &str) -> Result<String, Reply> {
    let perm = Permission { target: "tickets".to_string(), action: action.to_string() };
    match authz::authorize(&route.bearer, &perm) {
        Ok(p) => Ok(p.subject),
        Err(err) => {
            use crate::bindings::auth::identity::types::AuthError;
            let reply = match err {
                AuthError::InsufficientScope(_) => Reply::err(403, "forbidden"),
                AuthError::BackendUnavailable(_) | AuthError::Internal(_) => {
                    Reply::err(503, "auth_unavailable")
                }
                _ => Reply::err(401, "unauthenticated"),
            };
            Err(reply)
        }
    }
}

fn create_ticket(route: &Route, body: &str) -> Reply {
    if let Err(r) = authorize_perm(route, "write") {
        return r;
    }

    let req: Value = serde_json::from_str(body).unwrap_or(json!({}));
    let subject = req.get("subject").and_then(Value::as_str).unwrap_or("");
    let body_text = req.get("body").and_then(Value::as_str).unwrap_or("");
    let customer = req.get("customer").and_then(Value::as_str).unwrap_or("");

    if subject.is_empty() || body_text.is_empty() || !customer.starts_with("webhook:") {
        return Reply::err(400, "invalid_ticket");
    }

    let doc = json!({
        "subject": subject,
        "body": body_text,
        "customer": customer,
        "state": "open",
        "opened_at": guestfmt::rfc3339(now_secs())
    })
    .to_string();

    match records::create("tickets", &doc, &["state".to_string(), "customer".to_string()]) {
        Ok(e) => Reply::json(201, json!({"id": e.id})),
        Err(_) => Reply::err(500, "store_error"),
    }
}

fn get_ticket(route: &Route, id: &str) -> Reply {
    if let Err(r) = authorize_perm(route, "read") {
        return r;
    }

    match records::get("tickets", id) {
        Ok(e) => {
            let mut v: Value = serde_json::from_str(&e.data).unwrap_or(json!({}));
            if let Value::Object(ref mut m) = v {
                m.insert("id".to_string(), json!(e.id));
            }
            Reply::json(200, v)
        }
        Err(_) => Reply::err(404, "not_found"),
    }
}

fn list_tickets(route: &Route) -> Reply {
    if let Err(r) = authorize_perm(route, "read") {
        return r;
    }

    let state = route.param("state");
    let state = if state.is_empty() { "open".to_string() } else { state };
    let limit_str = route.param("limit");
    let limit = limit_str.parse::<u32>().unwrap_or(20).min(100) as usize;

    let mut entries =
        records::find_by("tickets", "state", &json!(state).to_string()).unwrap_or_default();

    entries.sort_by(|a, b| {
        let da: Value = serde_json::from_str(&a.data).unwrap_or(json!({}));
        let db: Value = serde_json::from_str(&b.data).unwrap_or(json!({}));
        let ta = da.get("opened_at").and_then(Value::as_str).unwrap_or("");
        let tb = db.get("opened_at").and_then(Value::as_str).unwrap_or("");
        ta.cmp(tb)
    });
    entries.truncate(limit);

    let items: Vec<Value> = entries
        .into_iter()
        .map(|e| {
            let mut v: Value = serde_json::from_str(&e.data).unwrap_or(json!({}));
            if let Value::Object(ref mut m) = v {
                m.insert("id".to_string(), json!(e.id));
            }
            v
        })
        .collect();

    Reply::json(200, json!({"tickets": items}))
}
