use serde_json::{json, Map, Value};

use crate::req;
use crate::{
    auth_types, bus, caller, now, orgs, personal_org, read_body, records, str_of, Incoming, Outcome,
};

fn session(
    request: &Incoming,
    query: &Map<String, Value>,
    role: orgs::Role,
) -> Result<(auth_types::Principal, String), Outcome> {
    let Some(p) = caller(request) else {
        return Err(Outcome::Err(401, "no session".into()));
    };
    match orgs::acting(&p.subject, &personal_org(&p), query, role) {
        Ok((org, _)) => Ok((p, org)),
        Err((code, msg)) => Err(Outcome::Err(code, msg)),
    }
}

/// Only a macro expanding in place can early-return from its CALLER.
macro_rules! session {
    ($request:expr, $query:expr, $role:expr) => {
        match session($request, $query, $role) {
            Ok(v) => v,
            Err(e) => return e,
        }
    };
}

/// One per project, so a consumer polls exactly that log instead of filtering a firehose.
fn goal_topic(project: &str) -> String {
    format!("goal.{project}")
}

const PROJECTS: &str = "projects";
const GOALS: &str = "goals";

/// Nothing leaves `failed` (a requeue makes a NEW goal, so what was tried stays visible).
fn goal_may(from: &str, to: &str) -> bool {
    matches!(
        (from, to),
        ("queued", "running")
            | ("queued", "abandoned")
            | ("running", "awaiting-human")
            | ("running", "failed")
            | ("running", "abandoned")
            | ("awaiting-human", "done")
            | ("awaiting-human", "failed")
            | ("awaiting-human", "abandoned")
    )
}

/// Safe inside a store name and a branch name.
fn valid_project_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 40
        && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-')
        && !name.ends_with('-')
}

pub fn project_create(request: &Incoming, query: &Map<String, Value>) -> Outcome {
    let (_p, org) = session!(request, query, orgs::Role::Member);
    let b: req::NewProject = match read_body(request)
        .map_err(|_| Outcome::Err(400, "could not read body".into()))
        .and_then(|raw| req::parse(&raw))
    {
        Ok(v) => v,
        Err(o) => return o,
    };
    if !valid_project_name(&b.name) {
        return Outcome::Err(
            422,
            "name must be 1-40 chars of [a-z0-9-], not starting or ending with -".into(),
        );
    }
    // `owner/name`, checked here rather than at the first forge call, where the answer is a misleading 404.
    if b.repo.split('/').filter(|s| !s.is_empty()).count() != 2 {
        return Outcome::Err(422, format!("repo must be \"owner/name\", got {:?}", b.repo));
    }
    if projects_of(&org).iter().any(|d| str_of(d, "name") == b.name) {
        return Outcome::Err(409, format!("project `{}` already exists", b.name));
    }

    let doc = json!({
        "name": b.name, "org": org, "repo": b.repo,
        "base": b.base.unwrap_or_else(|| "main".into()),
        "forge_token_ref": b.forge_token_ref.unwrap_or_default(),
        "llm_key_ref": b.llm_key_ref.unwrap_or_default(),
        "budget": b.budget.unwrap_or(0),
        // One at a time, which is the whole answer to concurrent pull requests (ADR-0082).
        "max_concurrent_runs": 1,
        "created": now(),
    });
    match records::create(PROJECTS, &doc.to_string(), &["org".to_string()]) {
        Ok(e) => Outcome::Json(
            201,
            json!({ "id": e.id, "name": doc["name"], "repo": doc["repo"], "base": doc["base"] })
                .to_string(),
        ),
        Err(e) => Outcome::Err(500, format!("recording the project: {e:?}")),
    }
}

fn projects_of(org: &str) -> Vec<Value> {
    records::find_by(PROJECTS, "org", &json!(org).to_string())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok())
        .collect()
}

pub fn projects_list(request: &Incoming, query: &Map<String, Value>) -> Outcome {
    let (_p, org) = session!(request, query, orgs::Role::Viewer);
    let rows: Vec<Value> = projects_of(&org)
        .into_iter()
        .map(|d| {
            let name = str_of(&d, "name");
            let goals = goals_of(&name);
            json!({
                "name": name, "repo": d["repo"], "base": d["base"],
                "queued": goals.iter().filter(|g| str_of(g, "state") == "queued").count(),
                "running": goals.iter().filter(|g| str_of(g, "state") == "running").count(),
                "failed": goals.iter().filter(|g| str_of(g, "state") == "failed").count(),
            })
        })
        .collect();
    Outcome::Json(200, json!({ "count": rows.len(), "projects": rows }).to_string())
}

fn goals_of(project: &str) -> Vec<Value> {
    records::find_by(GOALS, "project", &json!(project).to_string())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|e| {
            serde_json::from_str::<Value>(&e.data).ok().map(|mut v| {
                v["id"] = json!(e.id);
                v
            })
        })
        .collect()
}

pub fn goal_create(request: &Incoming, project: &str, query: &Map<String, Value>) -> Outcome {
    let (_p, org) = session!(request, query, orgs::Role::Member);
    if !projects_of(&org).iter().any(|d| str_of(d, "name") == project) {
        return Outcome::Err(404, format!("no project `{project}`"));
    }
    let b: req::NewGoal = match read_body(request)
        .map_err(|_| Outcome::Err(400, "could not read body".into()))
        .and_then(|raw| req::parse(&raw))
    {
        Ok(v) => v,
        Err(o) => return o,
    };
    if b.title.trim().is_empty() {
        return Outcome::Err(422, "a goal needs a title".into());
    }
    // Checked here, where the answer is a 422 naming the problem, not a cross-project child or an endless chain.
    let parent = b.parent.unwrap_or_default();
    if !parent.is_empty() {
        if let Err((code, msg)) = parent_is_usable(&parent, project) {
            return Outcome::Err(code, msg);
        }
    }
    let doc = json!({
        "project": project, "org": org,
        "title": b.title.trim(),
        "spec": b.spec.unwrap_or_default(),
        "priority": b.priority.unwrap_or(100),
        // A field, not a table: a sub-goal IS a goal, and a table would double every worklist query.
        "parent": parent,
        // Queued, and it stays there: a human starts every goal (ADR-0082); no loop drains this, on purpose.
        "state": "queued",
        "created": now(),
    });
    match records::create(GOALS, &doc.to_string(), &["project".to_string(), "org".to_string()]) {
        Ok(e) => {
            let event = json!({ "id": e.id, "project": project, "from": Value::Null, "to": "queued", "at": now() });
            let _ = bus::publish(&goal_topic(project), event.to_string().as_bytes());
            Outcome::Json(
                201,
                json!({ "id": e.id, "project": project, "state": "queued", "title": doc["title"] })
                    .to_string(),
            )
        }
        Err(e) => Outcome::Err(500, format!("recording the goal: {e:?}")),
    }
}

/// The walk catches a cycle; this catches a tree that never gets its leaves run.
const MAX_GOAL_DEPTH: usize = 8;

/// A chain is walked, not assumed: one closing on itself would hang every reader.
fn parent_is_usable(parent: &str, project: &str) -> Result<(), (u16, String)> {
    let Ok(entry) = records::get(GOALS, parent) else {
        return Err((422, format!("no goal `{parent}` to be a part of")));
    };
    let Ok(doc) = serde_json::from_str::<Value>(&entry.data) else {
        return Err((500, "the parent goal's record is unreadable".into()));
    };
    if str_of(&doc, "project") != project {
        return Err((
            422,
            format!(
                "goal `{parent}` belongs to project `{}`, not `{project}` — a part and the                  goal it serves live in one worklist",
                str_of(&doc, "project")
            ),
        ));
    }
    // `seen` turns an infinite loop into a message: a record edited around this check can still close a cycle.
    let mut seen = vec![parent.to_string()];
    let mut at = str_of(&doc, "parent");
    while !at.is_empty() {
        if seen.contains(&at) {
            return Err((
                422,
                format!("goal `{parent}` is already part of a cycle through `{at}`"),
            ));
        }
        seen.push(at.clone());
        // Bounded here as well as by the check below: this stops the LOOP, that stops the GOAL.
        if seen.len() > MAX_GOAL_DEPTH {
            break;
        }
        let Ok(up) = records::get(GOALS, &at) else { break };
        let Ok(updoc) = serde_json::from_str::<Value>(&up.data) else { break };
        at = str_of(&updoc, "parent");
    }
    // `seen` is the PARENT's chain, so a child sits one deeper.
    if seen.len() >= MAX_GOAL_DEPTH {
        return Err((
            422,
            format!(
                "goal `{parent}` is already {} levels deep, and {MAX_GOAL_DEPTH} is the bound \u{2014} a decomposition this deep is one nobody will reach the bottom of",
                seen.len()
            ),
        ));
    }
    Ok(())
}

pub fn goals_list(request: &Incoming, project: &str, query: &Map<String, Value>) -> Outcome {
    let (_p, _org) = session!(request, query, orgs::Role::Viewer);
    let want = query.get("state").and_then(|v| v.as_str()).unwrap_or_default();
    // `?parent=<id>` lists one goal's parts; `?parent=` (empty) lists only top-level goals; absent means every goal.
    let by_parent = query.get("parent").and_then(|v| v.as_str());
    let mut rows: Vec<Value> = goals_of(project)
        .into_iter()
        .filter(|g| want.is_empty() || str_of(g, "state") == want)
        .filter(|g| by_parent.is_none_or(|p| str_of(g, "parent") == p))
        .collect();
    // Priority first, then oldest — a worklist someone reads top-down.
    rows.sort_by(|a, b| {
        a["priority"]
            .as_i64()
            .unwrap_or(100)
            .cmp(&b["priority"].as_i64().unwrap_or(100))
            .then(str_of(a, "created").cmp(&str_of(b, "created")))
    });
    Outcome::Json(200, json!({ "count": rows.len(), "goals": rows }).to_string())
}

/// A lossy hint: every event is also a real write, so `goals_list` stays the answer. `group` defaults to
/// the caller's subject, so two scripts with no group do not steal each other's events.
pub fn events_list(request: &Incoming, project: &str, query: &Map<String, Value>) -> Outcome {
    let (p, _org) = session!(request, query, orgs::Role::Viewer);
    let group = query.get("group").and_then(|v| v.as_str()).unwrap_or(&p.subject);
    let max: u32 =
        query.get("max").and_then(|v| v.as_str()).and_then(|v| v.parse().ok()).unwrap_or(50);
    match bus::poll(&goal_topic(project), group, max) {
        Ok(events) => {
            let rows: Vec<Value> = events
                .iter()
                .map(|e| {
                    let payload =
                        serde_json::from_slice::<Value>(&e.payload).unwrap_or(Value::Null);
                    json!({ "id": e.id, "at": e.at, "event": payload })
                })
                .collect();
            Outcome::Json(
                200,
                json!({ "group": group, "count": rows.len(), "events": rows }).to_string(),
            )
        }
        // Never a hard failure: on any bus trouble a caller falls back to `goals_list`.
        Err(e) => Outcome::Err(503, format!("the event log is unavailable: {e:?}")),
    }
}

/// Never needed for correctness.
pub fn events_ack(request: &Incoming, project: &str, query: &Map<String, Value>) -> Outcome {
    let (_p, _org) = session!(request, query, orgs::Role::Viewer);
    let b: req::AckEvents = match read_body(request)
        .map_err(|_| Outcome::Err(400, "could not read body".into()))
        .and_then(|raw| req::parse(&raw))
    {
        Ok(v) => v,
        Err(o) => return o,
    };
    match bus::ack(&goal_topic(project), &b.group, &b.ids) {
        Ok(()) => Outcome::Json(200, json!({ "acked": b.ids.len() }).to_string()),
        Err(e) => Outcome::Err(503, format!("the event log is unavailable: {e:?}")),
    }
}

pub fn goal_transition(
    request: &Incoming,
    id: &str,
    to: &str,
    query: &Map<String, Value>,
) -> Outcome {
    let (_p, _org) = session!(request, query, orgs::Role::Member);
    let Ok(entry) = records::get(GOALS, id) else {
        return Outcome::Err(404, format!("no goal `{id}`"));
    };
    let Ok(mut doc) = serde_json::from_str::<Value>(&entry.data) else {
        return Outcome::Err(500, "the goal record is unreadable".into());
    };
    // Refused, not cascaded: a destructive multi-record operation should not be inferred from one click.
    if matches!(to, "done" | "abandoned") {
        let live: Vec<String> = goals_of(&str_of(&doc, "project"))
            .into_iter()
            .filter(|g| str_of(g, "parent") == id)
            .filter(|g| !matches!(str_of(g, "state").as_str(), "done" | "abandoned" | "failed"))
            .map(|g| str_of(&g, "title"))
            .collect();
        if !live.is_empty() {
            return Outcome::Err(
                409,
                format!(
                    "goal `{id}` still has {} unfinished part(s) — {}. Finish or abandon them                      first: parts of a `{to}` goal are rows nobody looks at again.",
                    live.len(),
                    live.join(", ")
                ),
            );
        }
    }

    let from = str_of(&doc, "state");
    if !goal_may(&from, to) {
        // Naming both ends beats "invalid transition": the caller usually has the wrong idea of where the goal IS.
        return Outcome::Err(409, format!("a goal cannot go from `{from}` to `{to}`"));
    }

    doc["state"] = json!(to);
    match to {
        "running" => {
            doc["started"] = json!(now());
            // A goal is FROZEN once it starts (ADR-0081): the spec it was judged against must not change under a run. Editing forks a new goal.
            doc["frozen_spec"] = doc["spec"].clone();
        }
        "failed" => {
            let reason = read_body(request)
                .ok()
                .and_then(|raw| req::parse::<req::FailGoal>(&raw).ok())
                .map(|b| b.reason)
                .unwrap_or_else(|| "no reason given".into());
            doc["reason"] = json!(reason);
            doc["failed_at"] = json!(now());
        }
        "awaiting-human" => {
            // Optional: records which pull request a human should look at.
            let pr = read_body(request)
                .ok()
                .and_then(|raw| req::parse::<req::ReviewGoal>(&raw).ok())
                .and_then(|b| b.pr);
            if let Some(pr) = pr {
                doc["pr"] = json!(pr);
            }
        }
        "done" => doc["finished"] = json!(now()),
        _ => {}
    }

    // Revision-guarded, so two people starting one goal cannot both win.
    match records::update(GOALS, id, &doc.to_string(), entry.revision) {
        Err(records::StoreError::RevisionConflict(_)) => {
            Outcome::Err(409, format!("`{id}` moved while you were looking at it — read it again"))
        }
        Ok(_) => {
            // A lossy hint (ADR-0081): errors are swallowed; a full bus must not stop a goal moving.
            let project = str_of(&doc, "project");
            let event =
                json!({ "id": id, "project": project, "from": from, "to": to, "at": now() });
            let _ = bus::publish(&goal_topic(&project), event.to_string().as_bytes());
            Outcome::Json(
                200,
                json!({ "id": id, "from": from, "state": to, "title": doc["title"] }).to_string(),
            )
        }
        Err(e) => Outcome::Err(500, format!("moving the goal: {e:?}")),
    }
}
