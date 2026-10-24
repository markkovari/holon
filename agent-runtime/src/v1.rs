//! `/v1`, the runtime's contract (ADR-0120, `api/holon/runtime/v1/runtime.proto`) as REST/JSON + SSE.

use std::sync::Arc;

use serde_json::{json, Value};
use tiny_http::Method;

use crate::runtime::Runtime;
use crate::spec::AgentSpec;
use crate::trace::Traceparent;

pub enum Reply {
    Json(u16, Value),
    Problem(u16, String),
    /// The event stream, from after this seq.
    Events(u64),
}

fn problem(status: u16, detail: impl Into<String>) -> Reply {
    Reply::Problem(status, detail.into())
}

pub fn problem_body(status: u16, detail: &str) -> Value {
    let title = match status {
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        409 => "Conflict",
        422 => "Unprocessable Content",
        423 => "Locked",
        429 => "Too Many Requests",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        _ => "Error",
    };
    json!({"type": "about:blank", "title": title, "status": status, "detail": detail})
}

fn agent_json(s: &AgentSpec, projects: &[crate::projects::Project]) -> Value {
    let member_of: Vec<&str> = projects
        .iter()
        .filter(|p| p.agents.iter().any(|a| a == &s.name))
        .map(|p| p.name.as_str())
        .collect();
    json!({
        "name": s.name,
        "description": s.description,
        "paused": s.paused,
        "spec": s,
        "model_label": s.model.label(),
        "projects": member_of,
    })
}

fn body_json(body: &str) -> Result<Value, Reply> {
    if body.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(body).map_err(|e| problem(400, format!("the body is not JSON: {e}")))
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array().into_iter().flatten().filter_map(|x| x.as_str().map(String::from)).collect()
}

/// `None` for a path that is not `/v1/...`. `segs` has the leading `v1` already removed.
pub fn route(
    rt: &Arc<Runtime>,
    method: &Method,
    segs: &[&str],
    query: &str,
    body: &str,
    authed: bool,
    trace: Option<Traceparent>,
) -> Reply {
    match (method, segs) {
        // The open tier, as the old trigger routes were: anything that may poke an agent may call these.
        (Method::Post, ["agents", name, "runs"]) => return start_run(rt, name, body, trace),
        (Method::Post, ["topics", topic, "events"]) => {
            let v = match body_json(body) {
                Ok(v) => v,
                Err(r) => return r,
            };
            let payload = v["payload"].as_str().unwrap_or_default();
            return match rt.emit_event(
                topic,
                payload,
                trace.as_ref().map(|t| t.trace_id.as_str()),
                trace.as_ref().map(|t| t.span_id.as_str()),
            ) {
                Ok(woken) => Reply::Json(202, json!({"woken": woken})),
                Err(e) => problem(422, e),
            };
        }
        _ => {}
    }
    if !authed {
        return problem(401, "the admin token is required");
    }
    let projects = || rt.store().list_projects();
    match (method, segs) {
        (Method::Get, ["agents"]) => {
            let ps = projects();
            let agents: Vec<Value> = rt.store().list().iter().map(|s| agent_json(s, &ps)).collect();
            Reply::Json(200, json!({"agents": agents}))
        }
        (Method::Get, ["agents", name]) if !name.contains(':') => match rt.store().get(name) {
            Some(s) => Reply::Json(200, agent_json(&s, &projects())),
            None => problem(404, format!("no agent named {name}")),
        },
        (Method::Put, ["agents", name]) => {
            let v = match body_json(body) {
                Ok(v) => v,
                Err(r) => return r,
            };
            let spec: AgentSpec = match serde_json::from_value(v["spec"].clone()) {
                Ok(s) => s,
                Err(e) => return problem(400, format!("not an agent spec: {e}")),
            };
            if spec.name != *name {
                return problem(400, "the spec's name is not the one in the path");
            }
            let exists = rt.store().get(name).is_some();
            let r = if exists { rt.update_agent(spec) } else { rt.create_agent(spec) };
            match (r, rt.store().get(name)) {
                (Ok(()), Some(s)) => {
                    Reply::Json(if exists { 200 } else { 201 }, agent_json(&s, &projects()))
                }
                (Ok(()), None) => problem(409, format!("{name} was saved and is gone")),
                (Err(e), _) => problem(422, e),
            }
        }
        (Method::Delete, ["agents", name]) => match rt.delete_agent(name) {
            Ok(()) => Reply::Json(200, json!({})),
            Err(e) => problem(404, e),
        },
        (Method::Post, ["agents", verb])
            if verb.ends_with(":pause") || verb.ends_with(":resume") =>
        {
            let (name, action) = verb.rsplit_once(':').unwrap_or_default();
            match rt.set_paused(name, action == "pause") {
                Ok(()) => match rt.store().get(name) {
                    Some(s) => Reply::Json(200, agent_json(&s, &projects())),
                    None => problem(404, format!("no agent named {name}")),
                },
                Err(e) => problem(404, e),
            }
        }
        (Method::Get, ["agents", name, "runs"]) => {
            let limit = crate::server::query_param(query, "limit")
                .and_then(|v| v.parse().ok())
                .unwrap_or(20);
            Reply::Json(200, json!({"runs": rt.store().runs(name, limit)}))
        }
        (Method::Get, ["events"]) => Reply::Events(
            crate::server::query_param(query, "after_seq")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0),
        ),
        (Method::Get, ["state"]) => {
            // Sessions as the stream's `session` event carries them, so a client replays them alike.
            let (seq, running, approvals, sessions) = rt.state();
            Reply::Json(
                200,
                json!({"seq": seq, "running": running, "approvals": approvals, "sessions": sessions}),
            )
        }
        (Method::Get, ["approvals"]) => {
            Reply::Json(200, json!({"approvals": rt.pending_approvals()}))
        }
        (Method::Post, ["approvals", id]) => {
            let v = match body_json(body) {
                Ok(v) => v,
                Err(r) => return r,
            };
            let Some(approve) = v["approve"].as_bool() else {
                return problem(400, "say `approve`: true or false");
            };
            let scope = v["scope"].as_str().filter(|s| !s.is_empty()).unwrap_or("once");
            if !matches!(scope, "once" | "task" | "session") {
                return problem(400, "`scope` is once, task or session");
            }
            match id.parse::<u64>() {
                Ok(id) if rt.resolve_approval_scoped(id, approve, scope) => {
                    Reply::Json(200, json!({}))
                }
                _ => problem(409, format!("no pending approval {id}")),
            }
        }
        (Method::Get, ["projects"]) => Reply::Json(200, json!({"projects": projects()})),
        (Method::Put, ["projects", name]) => {
            let v = match body_json(body) {
                Ok(v) => v,
                Err(r) => return r,
            };
            let mut p = rt.store().get_project(name).unwrap_or(crate::projects::Project {
                name: name.to_string(),
                ..Default::default()
            });
            if let Some(d) = v["description"].as_str() {
                p.description = d.to_string();
            }
            if let Some(l) = v.get("lead") {
                p.lead = l.as_str().filter(|l| !l.is_empty()).map(String::from);
            }
            match rt.put_project(p) {
                Ok(()) => match rt.store().get_project(name) {
                    Some(p) => Reply::Json(200, json!(p)),
                    None => problem(409, format!("{name} was saved and is gone")),
                },
                Err(e) => problem(422, e),
            }
        }
        (m @ (Method::Put | Method::Delete), ["projects", project, "agents", agent]) => {
            let r = if *m == Method::Put {
                rt.add_to_project(project, agent)
            } else {
                rt.remove_from_project(project, agent)
            };
            match (r, rt.store().get_project(project)) {
                (Ok(()), Some(p)) => Reply::Json(200, json!(p)),
                (Ok(()), None) => problem(404, format!("no project named {project}")),
                (Err(e), _) if e.starts_with("no project") || e.starts_with("no agent") => {
                    problem(404, e)
                }
                (Err(e), _) => problem(422, e),
            }
        }
        (Method::Get, ["keys"]) => match rt.keys() {
            Ok(k) => Reply::Json(200, json!({"keys": k})),
            Err(e) => problem(503, e),
        },
        (Method::Put, ["keys", name]) => {
            let v = match body_json(body) {
                Ok(v) => v,
                Err(r) => return r,
            };
            let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
            match rt.put_key(name, &s("host"), &s("key"), &s("header")) {
                Ok(()) => match rt.keys().map(|ks| ks.into_iter().find(|k| k.name == *name)) {
                    Ok(Some(k)) => Reply::Json(200, json!(k)),
                    Ok(None) => problem(409, format!("{name} was saved and is not listed")),
                    Err(e) => problem(503, e),
                },
                Err(e) => problem(422, e),
            }
        }
        (Method::Delete, ["keys", name]) => match rt.remove_key(name) {
            Ok(()) => Reply::Json(200, json!({})),
            Err(e) => problem(422, e),
        },
        (m @ (Method::Put | Method::Delete), ["keys", key, "agents", agent]) => {
            match rt.grant_key(key, agent, *m == Method::Put) {
                Ok(()) => match rt.store().get(agent) {
                    Some(s) => Reply::Json(200, agent_json(&s, &projects())),
                    None => problem(404, format!("no agent named {agent}")),
                },
                Err(e) => problem(422, e),
            }
        }
        _ => problem(404, "no such /v1 route"),
    }
}

fn start_run(rt: &Arc<Runtime>, name: &str, body: &str, trace: Option<Traceparent>) -> Reply {
    let v = match body_json(body) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let input = v["input"].as_str().unwrap_or_default().to_string();
    let why = v["why"].as_str().unwrap_or_default().to_string();
    let project = v["project"].as_str().filter(|p| !p.is_empty()).map(String::from);
    let attachments = strings(&v["attachments"]);
    if !v["wait"].as_bool().unwrap_or(false) {
        if rt.store().get(name).is_none() {
            return problem(404, format!("no agent named {name}"));
        }
        let (rt, agent) = (rt.clone(), name.to_string());
        // A run can wait minutes on an approval; its progress is on the stream.
        std::thread::spawn(move || {
            let _ =
                crate::server::start_run(&rt, &agent, &input, &why, project, attachments, trace);
        });
        return Reply::Json(202, json!({"agent": name, "status": "running"}));
    }
    match crate::server::start_run(rt, name, &input, &why, project, attachments, trace) {
        Ok(r) => Reply::Json(200, json!(r)),
        Err((status, e)) => problem(status, e),
    }
}
