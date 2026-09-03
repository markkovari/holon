//! Assign, transition, queue. The lifecycle lives in `fsm:workflow` and the
//! distance in `geo:resolve`; nothing here reimplements either.
//!
//! The one wrinkle: the seed writes documents straight to the store, so a request
//! can exist in state `assigned` with no fsm instance behind it. Rather than
//! branching on that, every entry point *replays* the document's state onto the
//! machine — fire the linear path from `new` and let the engine reject the steps
//! already taken. The engine is the only thing that knows what is legal, so it is
//! the only thing asked.

use crate::bindings::fsm::workflow::engine as fsm;
use crate::bindings::geo::resolve::coords as geo;
use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};
use serde_json::{json, Value};

const MACHINE: &str = "dispatch";

/// Contract roster. Compiled in, because there is no engineer collection.
const ROSTER: [(&str, f64, f64); 3] =
    [("ada", 47.4979, 19.0402), ("bela", 47.5316, 19.0430), ("cili", 47.4700, 19.0600)];

/// The path of events that lands an instance in each state, from `new`.
/// Replaying it is how a store-written document becomes a live instance.
const REPLAY: [(&str, &[&str]); 5] = [
    ("new", &[]),
    ("assigned", &["assign"]),
    ("enroute", &["assign", "depart"]),
    ("done", &["assign", "depart", "complete"]),
    ("cancelled", &["cancel"]),
];

fn definition() -> fsm::Definition {
    let t = |event: &str, source: &str, target: &str| fsm::Transition {
        event: event.into(),
        source: source.into(),
        target: target.into(),
    };
    fsm::Definition {
        states: ["new", "assigned", "enroute", "done", "cancelled"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        initial: "new".into(),
        transitions: vec![
            t("assign", "new", "assigned"),
            t("depart", "assigned", "enroute"),
            t("complete", "enroute", "done"),
            t("cancel", "new", "cancelled"),
            t("cancel", "assigned", "cancelled"),
        ],
        terminal: vec!["done".into(), "cancelled".into()],
    }
}

/// Make the machine and the instance exist, and make the instance agree with the
/// document. Already-defined / already-created / already-past-that-step all come
/// back as errors that mean "fine, carry on" — so they are all ignored the same way.
fn sync(id: &str, doc_state: &str) {
    let _ = fsm::define(MACHINE, &definition());
    let _ = fsm::create_instance(MACHINE, id);
    let arrived =
        |want: &str| fsm::get_status(MACHINE, id).map(|s| s.state == want).unwrap_or(false);
    if arrived(doc_state) {
        return;
    }
    let path = REPLAY.iter().find(|(s, _)| *s == doc_state).map(|(_, p)| *p).unwrap_or(&[]);
    for event in path {
        if arrived(doc_state) {
            return;
        }
        let _ = fsm::fire(MACHINE, id, event);
    }
}

fn load(id: &str) -> Option<(Value, u64)> {
    let e = records::get("requests", id).ok()?;
    let v: Value = serde_json::from_str(&e.data).ok()?;
    Some((v, e.revision))
}

fn with_id(doc: &Value, id: &str) -> Value {
    let mut out = doc.clone();
    if let Some(m) = out.as_object_mut() {
        m.insert("id".into(), json!(id));
    }
    out
}

fn state_of(doc: &Value) -> String {
    doc.get("state").and_then(Value::as_str).unwrap_or("new").to_string()
}

fn conflict(err: fsm::FsmError, fallback: &str) -> Reply {
    let state = match err {
        fsm::FsmError::IllegalTransition(current) => current,
        _ => fallback.to_string(),
    };
    Reply::json(409, json!({ "error": "illegal_transition", "state": state }))
}

fn save(id: &str, doc: &Value, revision: u64) -> Result<(), Reply> {
    match records::update("requests", id, &doc.to_string(), revision) {
        Ok(_) => Ok(()),
        Err(_) => Err(Reply::err(500, "store_failed")),
    }
}

fn assign(id: &str) -> Reply {
    let Some((mut doc, revision)) = load(id) else {
        return Reply::err(404, "not_found");
    };
    let current = state_of(&doc);
    sync(id, &current);

    let here = geo::Point {
        lat: doc.get("lat").and_then(Value::as_f64).unwrap_or(0.0),
        lon: doc.get("lon").and_then(Value::as_f64).unwrap_or(0.0),
    };
    let mut best: Option<(&str, f64)> = None;
    for (name, lat, lon) in ROSTER {
        match geo::distance_meters(here, geo::Point { lat, lon }) {
            Ok(d) if best.map_or(true, |(_, b)| d < b) => best = Some((name, d)),
            Ok(_) => {}
            Err(_) => return Reply::err(400, "bad_coordinate"),
        }
    }
    let Some((engineer, metres)) = best else {
        return Reply::err(500, "no_engineer");
    };

    let status = match fsm::fire(MACHINE, id, "assign") {
        Ok(s) => s,
        Err(e) => return conflict(e, &current),
    };

    doc["engineer"] = json!(engineer);
    doc["distance_m"] = json!(metres.round() as i64);
    doc["state"] = json!(status.state);
    if let Err(r) = save(id, &doc, revision) {
        return r;
    }
    Reply::json(200, with_id(&doc, id))
}

fn transition(id: &str, body: &str) -> Reply {
    let event = serde_json::from_str::<Value>(body)
        .ok()
        .and_then(|v| v.get("event").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_default();
    if !["depart", "complete", "cancel"].contains(&event.as_str()) {
        return Reply::err(400, "invalid");
    }
    let Some((mut doc, revision)) = load(id) else {
        return Reply::err(404, "not_found");
    };
    let current = state_of(&doc);
    sync(id, &current);

    let status = match fsm::fire(MACHINE, id, &event) {
        Ok(s) => s,
        Err(e) => return conflict(e, &current),
    };
    doc["state"] = json!(status.state);
    if let Err(r) = save(id, &doc, revision) {
        return r;
    }
    Reply::json(200, with_id(&doc, id))
}

fn queue() -> Reply {
    let mut open: Vec<(i64, String, Value)> = Vec::new();
    let mut after = String::new();
    loop {
        let page = match records::list_records("requests", 100, &after) {
            Ok(p) => p,
            Err(_) => return Reply::err(500, "store_failed"),
        };
        for e in &page.entries {
            let Ok(doc) = serde_json::from_str::<Value>(&e.data) else { continue };
            let state = state_of(&doc);
            if state == "done" || state == "cancelled" {
                continue;
            }
            let d = doc.get("distance_m").and_then(Value::as_i64).unwrap_or(0);
            open.push((d, e.id.clone(), with_id(&doc, &e.id)));
        }
        if page.next.is_empty() {
            break;
        }
        after = page.next;
    }
    open.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    Reply::json(200, json!({ "queue": open.into_iter().map(|(_, _, d)| d).collect::<Vec<_>>() }))
}

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "requests", id, "assign"]) => assign(id),
        (Method::Post, ["api", "requests", id, "transition"]) => transition(id, body),
        (Method::Get, ["api", "queue"]) => queue(),
        _ => Reply::err(405, "method_not_allowed"),
    }
}