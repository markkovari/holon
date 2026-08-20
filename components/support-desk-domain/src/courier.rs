//! `courier` — the only part that talks to the far end.
//!
//! What happens when a send FAILS is the whole of this part. Every `Err` from `notify::send`
//! is treated identically as a failed delivery (a refusal and an unreachable host cannot be
//! told apart, and trying to guess drops replies over outages that would have retried). What
//! matters is never re-checked from the send result — it is read from `outbox::fail`'s return
//! value, which is the only thing that says whether this event will ever be retried again.

use crate::bindings::auth::identity::types::{AuthError, Permission};
use crate::bindings::auth::identity::authorizer as authz;
use crate::bindings::notify::dispatch::dispatcher as notify;
use crate::bindings::outbox::dispatch::queue as outbox;
use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};
use serde_json::{json, Value};

/// `tickets:deliver` on every route this part owns.
fn authorize(route: &Route) -> Result<(), Reply> {
    if route.bearer.is_empty() {
        return Err(Reply::err(401, "unauthenticated"));
    }
    let required = Permission { target: "tickets".to_string(), action: "deliver".to_string() };
    match authz::authorize(&route.bearer, &required) {
        Ok(_) => Ok(()),
        Err(AuthError::InvalidToken(_)) | Err(AuthError::Expired) | Err(AuthError::Malformed(_)) => {
            Err(Reply::err(401, "unauthenticated"))
        }
        Err(AuthError::InsufficientScope(_)) => Err(Reply::err(403, "forbidden")),
        Err(AuthError::BackendUnavailable(_)) | Err(AuthError::Internal(_)) => {
            Err(Reply::err(503, "auth_unavailable"))
        }
        Err(_) => Err(Reply::err(401, "unauthenticated")),
    }
}

pub fn handle(method: &Method, route: &Route, _body: &str) -> Reply {
    if let Err(reply) = authorize(route) {
        return reply;
    }
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "deliver"]) => deliver(route),
        (Method::Get, ["api", "dead-letters"]) => dead_letters(route),
        (Method::Post, ["api", "dead-letters", id, "replay"]) => replay(id),
        _ => Reply::err(404, "not_found"),
    }
}

/// One delivery pass: claim, send, and act on exactly what the outbox reports back.
fn deliver(route: &Route) -> Reply {
    let max = route.param("max").parse::<u32>().ok().unwrap_or(10).min(50);

    let events = match outbox::claim(max, 30) {
        Ok(events) => events,
        Err(_) => return Reply::err(503, "outbox_unavailable"),
    };

    let claimed = events.len();
    let mut delivered = 0u32;
    let mut failed = 0u32;
    let mut dead = 0u32;

    for event in events {
        let payload: Value = match serde_json::from_slice(&event.payload) {
            Ok(v) => v,
            // Nothing to send this into — it did not fail delivery, but it also
            // cannot be delivered, so it goes through the same fail path so it
            // eventually dead-letters instead of being claimed forever.
            Err(_) => {
                failed += 1;
                note_fail(&event.id, &mut dead);
                continue;
            }
        };
        let target = payload
            .get("target")
            .and_then(Value::as_str)
            .unwrap_or("")
            .strip_prefix("webhook:")
            .unwrap_or("")
            .to_string();
        // The far end is a webhook receiver, not an inbox: it needs a parseable
        // document, not a bare string. Send the whole enqueued payload (ticket,
        // target, subject, body) as the wire body so the receiver can `json.loads`
        // it — sending just the draft text arrives as un-quoted plain text, which
        // is not valid JSON at all.
        let msg = notify::Message {
            channel: notify::Channel::Webhook,
            target,
            subject: payload.get("subject").and_then(Value::as_str).unwrap_or("").to_string(),
            body: payload.to_string(),
        };

        // The whole part: `send`'s Ok/Err says whether it arrived, nothing more —
        // never re-inspected. Only ack what was actually sent, only fail what
        // wasn't, and always read what `fail` returns.
        match notify::send(&msg) {
            Ok(_) => {
                let _ = outbox::ack(&event.id);
                delivered += 1;
            }
            Err(_) => {
                failed += 1;
                note_fail(&event.id, &mut dead);
            }
        }
    }

    Reply::json(
        200,
        json!({ "claimed": claimed, "delivered": delivered, "failed": failed, "dead": dead }),
    )
}

/// Calls `outbox::fail` and counts it as dead if that's the state it reports back —
/// the one call in this file that a courier ignoring it would abandon a reply silently.
fn note_fail(id: &str, dead: &mut u32) {
    if let Ok(outbox::State::Dead) = outbox::fail(id) {
        *dead += 1;
    }
}

fn dead_letters(route: &Route) -> Reply {
    let max = route.param("max").parse::<u32>().ok().unwrap_or(20).min(100);
    match outbox::dead_letters(max) {
        Ok(events) => {
            let list: Vec<Value> = events
                .iter()
                .map(|e| {
                    json!({
                        "id": e.id,
                        "topic": e.topic,
                        "attempts": e.attempts,
                        "payload": serde_json::from_slice::<Value>(&e.payload).unwrap_or(Value::Null),
                    })
                })
                .collect();
            Reply::json(200, json!({ "events": list }))
        }
        Err(_) => Reply::err(503, "outbox_unavailable"),
    }
}

fn replay(id: &str) -> Reply {
    match outbox::replay(id) {
        Ok(()) => Reply::no_content(),
        Err(_) => Reply::err(404, "not_found"),
    }
}