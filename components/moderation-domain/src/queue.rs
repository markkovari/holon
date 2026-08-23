//! `queue` — the rules, what is waiting, and what has left.
//!
//! The rules live in `policy:guard` and are read back out of it — never copied into a
//! second place, because a copy is exactly the thing that drifts the moment anything
//! else (the router's `/test/rules` fixture, mid-run) writes a rule.

use crate::bindings::auth::identity::authorizer as authz;
use crate::bindings::auth::identity::types as auth_types;
use crate::bindings::event::bus::bus;
use crate::bindings::policy::guard::guard as policy;
use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use crate::{cfg, Reply, Route};
use serde_json::{json, Value};

/// Auth for every route this part owns: `items:moderate` for the rule routes,
/// `items:read` for the queue and event routes. Mapped exactly per CONTRACT.md's
/// error table — a missing/empty header never reaches `authorize` at all.
fn require(route: &Route, action: &str) -> Result<(), Reply> {
    if route.bearer.is_empty() {
        return Err(Reply::err(401, "unauthenticated"));
    }
    let required =
        auth_types::Permission { target: "items".to_string(), action: action.to_string() };
    match authz::authorize(&route.bearer, &required) {
        Ok(_) => Ok(()),
        Err(auth_types::AuthError::InsufficientScope(_)) => Err(Reply::err(403, "forbidden")),
        Err(auth_types::AuthError::BackendUnavailable(_))
        | Err(auth_types::AuthError::Internal(_)) => Err(Reply::err(503, "auth_unavailable")),
        Err(_) => Err(Reply::err(401, "unauthenticated")),
    }
}

fn parse_effect(s: &str) -> Option<policy::Effect> {
    match s {
        "allow" => Some(policy::Effect::Allow),
        "deny" => Some(policy::Effect::Deny),
        _ => None,
    }
}

fn effect_str(e: &policy::Effect) -> &'static str {
    match e {
        policy::Effect::Allow => "allow",
        policy::Effect::Deny => "deny",
    }
}

fn parse_op(s: &str) -> Option<policy::Op> {
    match s {
        "eq" => Some(policy::Op::Eq),
        "ne" => Some(policy::Op::Ne),
        "in-list" => Some(policy::Op::InList),
        "lt" => Some(policy::Op::Lt),
        "gt" => Some(policy::Op::Gt),
        "has" => Some(policy::Op::Has),
        _ => None,
    }
}

fn op_str(o: &policy::Op) -> &'static str {
    match o {
        policy::Op::Eq => "eq",
        policy::Op::Ne => "ne",
        policy::Op::InList => "in-list",
        policy::Op::Lt => "lt",
        policy::Op::Gt => "gt",
        policy::Op::Has => "has",
    }
}

/// `{"rules":[{"id","action","effect","priority","conditions":[{"left","op","right"}]}]}`.
/// An unknown `effect`/`op` is `400 invalid_rule` here, before `set_rules` ever sees it —
/// a rule the engine would reject later is a rule nobody wrote down.
fn parse_rules(body: &str) -> Option<Vec<policy::Rule>> {
    let req: Value = serde_json::from_str(body).ok()?;
    let arr = req.get("rules")?.as_array()?;
    let mut rules = Vec::with_capacity(arr.len());
    for r in arr {
        let id = r.get("id").and_then(Value::as_str)?.to_string();
        let action = r.get("action").and_then(Value::as_str)?.to_string();
        let effect = parse_effect(r.get("effect").and_then(Value::as_str)?)?;
        let priority = r.get("priority").and_then(Value::as_u64).unwrap_or(0) as u32;
        let conds = r.get("conditions").and_then(Value::as_array)?;
        let mut conditions = Vec::with_capacity(conds.len());
        for c in conds {
            let left = c.get("left").and_then(Value::as_str)?.to_string();
            let right = c.get("right").and_then(Value::as_str)?.to_string();
            let op = parse_op(c.get("op").and_then(Value::as_str)?)?;
            conditions.push(policy::Condition { left, op, right });
        }
        rules.push(policy::Rule { id, action, effect, conditions, priority });
    }
    Some(rules)
}

fn post_rules(route: &Route, body: &str) -> Reply {
    if let Err(e) = require(route, "moderate") {
        return e;
    }
    let Some(rules) = parse_rules(body) else {
        return Reply::err(400, "invalid_rule");
    };
    match policy::set_rules(&cfg("policy-domain", "moderation"), &rules) {
        Ok(()) => Reply::no_content(),
        Err(_) => Reply::err(503, "policy_unavailable"),
    }
}

/// Read straight back through `get_rules` — never a copy this part kept itself.
fn get_rules(route: &Route) -> Reply {
    if let Err(e) = require(route, "moderate") {
        return e;
    }
    match policy::get_rules(&cfg("policy-domain", "moderation")) {
        Ok(rules) => {
            let out: Vec<Value> = rules
                .iter()
                .map(|r| {
                    json!({
                        "id": r.id,
                        "action": r.action,
                        "effect": effect_str(&r.effect),
                        "priority": r.priority,
                        "conditions": r.conditions.iter().map(|c| json!({
                            "left": c.left,
                            "op": op_str(&c.op),
                            "right": c.right,
                        })).collect::<Vec<_>>(),
                    })
                })
                .collect();
            Reply::json(200, json!({ "rules": out }))
        }
        Err(_) => Reply::err(503, "policy_unavailable"),
    }
}

/// `?state=&limit=`. `state` defaults to `"pending"` and is an index lookup — `find_by`
/// wants the value JSON-encoded, not the bare word. `limit` defaults to 20, capped at 100.
/// `find_by` has no limit of its own, so entries are sorted (ids are sortable ULIDs —
/// oldest first, a queue not a stack) and truncated here.
fn get_queue(route: &Route) -> Reply {
    if let Err(e) = require(route, "read") {
        return e;
    }
    let state = {
        let s = route.param("state");
        if s.is_empty() {
            "pending".to_string()
        } else {
            s
        }
    };
    let limit: usize = {
        let s = route.param("limit");
        let n: u32 = if s.is_empty() { 20 } else { s.parse().unwrap_or(20) };
        n.min(100) as usize
    };
    let value = serde_json::to_string(&state).unwrap_or_else(|_| "\"pending\"".to_string());
    match records::find_by("items", "state", &value) {
        Ok(mut entries) => {
            entries.sort_by(|a, b| a.id.cmp(&b.id));
            entries.truncate(limit);
            let items: Vec<Value> = entries
                .iter()
                .map(|e| {
                    let mut v: Value = serde_json::from_str(&e.data).unwrap_or(json!({}));
                    if let Some(obj) = v.as_object_mut() {
                        obj.insert("id".to_string(), json!(e.id));
                    }
                    v
                })
                .collect();
            Reply::json(200, json!({ "items": items }))
        }
        Err(_) => Reply::err(503, "store_unavailable"),
    }
}

/// `?topic=&max=`. What has left the system, over the bus — a read, not a consume:
/// no `ack` here, or a reviewer polling twice would see each decision once.
fn get_events(route: &Route) -> Reply {
    if let Err(e) = require(route, "read") {
        return e;
    }
    let topic = {
        let t = route.param("topic");
        if t.is_empty() {
            "moderation.decided".to_string()
        } else {
            t
        }
    };
    let max: u32 = {
        let s = route.param("max");
        if s.is_empty() {
            20
        } else {
            s.parse().unwrap_or(20)
        }
    };
    match bus::poll(&topic, "queue-reader", max) {
        Ok(events) => Reply::json(
            200,
            json!({
                "events": events.iter().map(|e| json!({
                    "id": e.id,
                    "topic": e.topic,
                    "at": e.at,
                    "payload": serde_json::from_slice::<Value>(&e.payload).unwrap_or(json!(null)),
                })).collect::<Vec<_>>()
            }),
        ),
        Err(_) => Reply::err(503, "bus_unavailable"),
    }
}

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "rules"]) => post_rules(route, body),
        (Method::Get, ["api", "rules"]) => get_rules(route),
        (Method::Get, ["api", "queue"]) => get_queue(route),
        (Method::Get, ["api", "events"]) => get_events(route),
        _ => Reply::err(404, "not_found"),
    }
}
