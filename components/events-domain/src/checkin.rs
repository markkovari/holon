//! `checkin` — the organizer scans a code at the door.
//!
//! Small, and one thing about it is not: the refusal to admit the same ticket
//! twice comes from the state machine, carrying the state the machine is actually
//! in. An `if state == "checked-in"` in this file would agree with the machine
//! right up until anything else moved a ticket — a swap, a release — and then
//! silently stop agreeing.
//!
//! The lifecycle is registered here rather than at startup because a component has
//! no startup: it is a handler, and the first request is the first moment anything
//! runs. `define` on a machine that already exists is not an error (see fsm.wit),
//! which is what makes calling it per request the honest thing rather than a waste.

use serde_json::json;

use crate::bindings::fsm::workflow::engine as fsm;
use crate::bindings::wasi::http::types::Method;
use crate::store::{find_by_str, load, save, with_id};
use crate::{has_role, require, Reply, Route};

pub const MACHINE: &str = "ticket";

/// The lifecycle, as a definition. Idempotent, so every entry point may call it.
fn ensure_machine() {
    let _ = fsm::define(
        MACHINE,
        &fsm::Definition {
            states: vec!["issued".into(), "checked-in".into(), "released".into()],
            initial: "issued".into(),
            transitions: vec![
                fsm::Transition {
                    event: "check-in".into(),
                    source: "issued".into(),
                    target: "checked-in".into(),
                },
                fsm::Transition {
                    event: "release".into(),
                    source: "issued".into(),
                    target: "released".into(),
                },
            ],
            terminal: vec!["released".into()],
        },
    );
}

/// Move one ticket, and give the CURRENT state back on refusal.
///
/// `Err` carries the state the machine reports, which is exactly what a 409 body
/// needs — so no caller has to look it up, and none can report a different one.
/// Shared with `tickets::release`, because two parts firing events at one machine
/// through two code paths is how the two stop agreeing.
pub fn fire(ticket_id: &str, event: &str) -> Result<fsm::Status, String> {
    ensure_machine();
    // Create it only if it is NOT there. `create-instance` says so in fsm.wit:
    // "Re-creating an existing instance RESETS it to initial." Calling it
    // unconditionally put a checked-in ticket back to `issued` on every scan, so
    // the door admitted the same code twice and answered 200 both times — a
    // turnstile that counts one person forever. `get-status` is the existence
    // check, and its failure is the only thing that should lead to a create.
    if fsm::get_status(MACHINE, ticket_id).is_err() {
        let _ = fsm::create_instance(MACHINE, ticket_id);
    }
    match fsm::fire(MACHINE, ticket_id, event) {
        Ok(s) => Ok(s),
        Err(fsm::FsmError::IllegalTransition(state)) => Err(state),
        Err(_) => Err(String::new()),
    }
}

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "checkin"]) => scan(route, body),
        _ => Reply::err(404, "not_found"),
    }
}

fn scan(route: &Route, body: &str) -> Reply {
    let p = match require(route, "checkin", "write") {
        Ok(p) => p,
        Err(r) => return r,
    };
    let Ok(input) = serde_json::from_str::<serde_json::Value>(body) else {
        return Reply::err(400, "malformed_body");
    };
    let code = input["code"].as_str().unwrap_or_default();
    if code.is_empty() {
        return Reply::err(400, "invalid");
    }

    // The scanner sends the decoded string; decoding the image is the browser's job.
    let Some(entry) = find_by_str("tickets", "code", code).into_iter().next() else {
        return Reply::err(404, "no_such_ticket");
    };
    let mut doc = with_id(&entry);

    // The door is the event's organizer, not any organizer.
    let event_id = doc["event_id"].as_str().unwrap_or_default().to_string();
    let mine = match load("events", &event_id) {
        Ok((_, ev)) => ev["organizer"].as_str() == Some(p.subject.as_str()),
        Err(_) => false,
    };
    if !(mine || has_role(&p, "admin")) {
        return Reply::err(403, "forbidden");
    }

    match fire(&entry.id, "check-in") {
        Ok(status) => {
            doc["state"] = json!(status.state);
            doc["checked_in_at"] = json!(true);
            if let Err(r) = save("tickets", &entry, &doc) {
                return r;
            }
            Reply::json(
                200,
                json!({
                    "ticket_id": entry.id,
                    "event_id": event_id,
                    "holder": doc["holder"],
                    "state": status.state,
                }),
            )
        }
        Err(state) => {
            let code = if state == "checked-in" { "already_checked_in" } else { "not_admissible" };
            Reply::json(409, json!({ "error": code, "state": state }))
        }
    }
}
