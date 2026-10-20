//! `MOVES` is the lifecycle, written once. `fire` goes first and its typed errors say what is
//! missing, so an instance is never defensively re-created (which would reset it to `open`).
use crate::bindings::fsm::workflow::engine as fsm;
use crate::bindings::p3::http::types::Method;
use crate::bindings::records::store::store as records;
use crate::{Reply, Route};
use serde_json::{json, Value};

const MACHINE: &str = "report";
const REPORTS: &str = "reports";
const STATES: [&str; 4] = ["open", "triaged", "fixed", "closed"];
const TERMINAL: &str = "closed";
const SEVERITIES: [&str; 3] = ["high", "medium", "low"];

const MOVES: &[(&str, &str, &str)] = &[
    ("triage", "open", "triaged"),
    ("fix", "triaged", "fixed"),
    ("close", "fixed", TERMINAL),
    ("close", "triaged", TERMINAL),
    ("close", "open", TERMINAL),
];

fn definition() -> fsm::Definition {
    fsm::Definition {
        states: STATES.iter().map(|s| s.to_string()).collect(),
        initial: STATES[0].to_string(),
        transitions: MOVES
            .iter()
            .map(|(e, s, t)| fsm::Transition {
                event: e.to_string(),
                source: s.to_string(),
                target: t.to_string(),
            })
            .collect(),
        terminal: vec![TERMINAL.to_string()],
    }
}

/// A cold fsm store with warm documents would otherwise silently rewind a report to `open`.
fn catch_up(id: &str, want: &str) -> Result<(), fsm::FsmError> {
    let mut at = STATES[0];
    while at != want {
        let step = MOVES
            .iter()
            .find(|(_, s, t)| *s == at && *t == want)
            .or_else(|| MOVES.iter().find(|(_, s, t)| *s == at && *t != TERMINAL));
        let Some((event, _, target)) = step else { return Ok(()) };
        fsm::fire(MACHINE, id, event)?;
        at = target;
    }
    Ok(())
}

/// At most one `define` and one `create_instance` can be needed, so three attempts is the ceiling.
fn fire(id: &str, event: &str, doc_state: &str) -> Result<fsm::Status, fsm::FsmError> {
    for _ in 0..3 {
        match fsm::fire(MACHINE, id, event) {
            Err(fsm::FsmError::UnknownMachine) => fsm::define(MACHINE, &definition())?,
            Err(fsm::FsmError::UnknownInstance) => {
                fsm::create_instance(MACHINE, id)?;
                catch_up(id, doc_state)?;
            }
            settled => return settled,
        }
    }
    fsm::fire(MACHINE, id, event)
}

fn transition(id: &str, body: &str) -> Reply {
    let Ok(entry) = records::get(REPORTS, id) else { return Reply::err(404, "not_found") };
    let Ok(mut doc) = serde_json::from_str::<Value>(&entry.data) else {
        return Reply::err(500, "store_failed");
    };
    let Ok(req) = serde_json::from_str::<Value>(body) else { return Reply::err(400, "invalid") };

    let event = req.get("event").and_then(Value::as_str).unwrap_or_default();
    if !MOVES.iter().any(|(e, _, _)| *e == event) {
        return Reply::err(400, "invalid");
    }
    let severity = req.get("severity").and_then(Value::as_str).unwrap_or_default();
    if event == "triage" && !SEVERITIES.contains(&severity) {
        return Reply::err(400, "invalid");
    }

    let doc_state = doc.get("state").and_then(Value::as_str).unwrap_or(STATES[0]).to_string();
    let status = match fire(id, event, &doc_state) {
        Ok(s) => s,
        Err(fsm::FsmError::IllegalTransition(state)) => {
            return Reply::json(409, json!({ "error": "illegal", "state": state }))
        }
        Err(_) => return Reply::err(503, "fsm_unavailable"),
    };

    doc["state"] = json!(status.state);
    if event == "triage" {
        doc["severity"] = json!(severity);
    }
    if records::update(REPORTS, id, &doc.to_string(), entry.revision).is_err() {
        return Reply::err(500, "store_failed");
    }

    let mut out = json!({ "id": id, "state": status.state });
    if let Some(s) = doc.get("severity") {
        out["severity"] = s.clone();
    }
    Reply::json(200, out)
}

fn queue() -> Reply {
    let mut ranked: Vec<(usize, String, String, Value)> = Vec::new();
    let mut cursor = String::new();
    loop {
        let Ok(page) = records::list_records(REPORTS, 200, &cursor) else {
            return Reply::err(500, "store_failed");
        };
        for e in &page.entries {
            let Ok(d) = serde_json::from_str::<Value>(&e.data) else { continue };
            let state = d.get("state").and_then(Value::as_str).unwrap_or(STATES[0]);
            if state == TERMINAL {
                continue;
            }
            let severity = d.get("severity").and_then(Value::as_str);
            let mut row = json!({
                "id": e.id,
                "title": d.get("title").and_then(Value::as_str).unwrap_or_default(),
                "component": d.get("component").and_then(Value::as_str).unwrap_or_default(),
                "state": state,
            });
            if let Some(s) = severity {
                row["severity"] = json!(s);
            }
            let rank = severity
                .and_then(|s| SEVERITIES.iter().position(|k| *k == s))
                .unwrap_or(SEVERITIES.len());
            let at = d.get("reported_at").and_then(Value::as_str).unwrap_or_default().to_string();
            ranked.push((rank, at, e.id.clone(), row));
        }
        if page.next.is_empty() {
            break;
        }
        cursor = page.next;
    }
    ranked.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    Reply::json(200, json!({ "queue": ranked.into_iter().map(|r| r.3).collect::<Vec<_>>() }))
}

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match seg.as_slice() {
        ["api", "queue"] if matches!(method, Method::Get) => queue(),
        ["api", "reports", id, "transition"] if matches!(method, Method::Post) => {
            transition(id, body)
        }
        _ => Reply::err(404, "not_found"),
    }
}
