use crate::bindings::auth::identity::authorizer as authz;
use crate::bindings::auth::identity::types::Permission;
use crate::bindings::notify::dispatch::dispatcher as notify;
use crate::bindings::outbox::dispatch::queue as outbox;
use crate::bindings::wasi::http::types::Method;
use crate::{cfg_u64, now_secs, Reply, Route};
use serde_json::{json, Value};

pub fn handle(method: &Method, route: &Route, _body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "deliver"]) => deliver(route),
        (Method::Get, ["api", "dead-letters"]) => list_dead_letters(route),
        (Method::Post, ["api", "dead-letters", id, "replay"]) => replay_dead_letter(route, id),
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

fn deliver(route: &Route) -> Reply {
    if let Err(r) = authorize_perm(route, "deliver") {
        return r;
    }

    let max_str = route.param("max");
    let max = max_str.parse::<u32>().unwrap_or(10).min(50);

    let events = match outbox::claim(max, 30) {
        Ok(e) => e,
        Err(_) => return Reply::err(503, "outbox_unavailable"),
    };

    let claimed = events.len();
    let mut delivered = 0;
    let mut failed = 0;
    let mut dead = 0;

    for event in events {
        let payload: Value = serde_json::from_slice(&event.payload).unwrap_or(json!({}));
        let target = payload.get("target").and_then(Value::as_str).unwrap_or("");
        let subject = payload.get("subject").and_then(Value::as_str).unwrap_or("");

        let stripped_target =
            if let Some(t) = target.strip_prefix("webhook:") { t } else { target };

        let msg_body = String::from_utf8_lossy(&event.payload).to_string();

        let msg = notify::Message {
            channel: notify::Channel::Webhook,
            target: stripped_target.to_string(),
            subject: subject.to_string(),
            body: msg_body,
        };

        match notify::send(&msg) {
            Ok(_) => {
                let _ = outbox::ack(&event.id);
                delivered += 1;
            }
            Err(_) => {
                failed += 1;
                if let Ok(state) = outbox::fail(&event.id) {
                    if matches!(state, outbox::State::Dead) {
                        dead += 1;
                    }
                }
            }
        }
    }

    Reply::json(
        200,
        json!({
            "claimed": claimed,
            "delivered": delivered,
            "failed": failed,
            "dead": dead
        }),
    )
}

fn list_dead_letters(route: &Route) -> Reply {
    if let Err(r) = authorize_perm(route, "deliver") {
        return r;
    }
    let max_str = route.param("max");
    let max = max_str.parse::<u32>().unwrap_or(20).min(100);

    match outbox::dead_letters(max) {
        Ok(events) => {
            let json_events: Vec<Value> = events
                .into_iter()
                .map(|e| {
                    let payload =
                        serde_json::from_slice::<Value>(&e.payload).unwrap_or(json!(null));
                    json!({
                        "id": e.id,
                        "topic": e.topic,
                        "attempts": e.attempts,
                        "payload": payload
                    })
                })
                .collect();
            Reply::json(200, json!({"events": json_events}))
        }
        Err(_) => Reply::err(503, "outbox_unavailable"),
    }
}

fn replay_dead_letter(route: &Route, id: &str) -> Reply {
    if let Err(r) = authorize_perm(route, "deliver") {
        return r;
    }
    match outbox::replay(id) {
        Ok(_) => Reply::no_content(),
        Err(outbox::OutboxError::NotFound) => Reply::err(404, "not_found"),
        Err(_) => Reply::err(503, "outbox_unavailable"),
    }
}
