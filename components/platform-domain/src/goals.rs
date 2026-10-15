//! Projects and their queues — ADR-0082's half of the platform.
//!
//! Split out of `lib.rs` because it answers a different question: what is TO BE DONE, where the rest is
//! about what is DEPLOYED. The lifecycle is a table rather than scattered `if state == …` checks: nothing
//! leaves `failed` (a requeue makes a NEW goal) and nothing reaches `done` without having run.

use serde_json::{json, Map, Value};

use crate::req;
use crate::{
    auth_types, bus, caller, now, orgs, personal_org, read_body, records, str_of, Incoming, Outcome,
};

/// Every route here needs a session and the org the caller acts as, differing only in the required
/// `orgs::Role`. A plain function because the *check* is one place's business, though the *early return*
/// has to happen at each call site (see `session!` below).
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

/// `let (p, org) = session!(request, query, orgs::Role::Member);` — only a macro expanding in place can early-return from its CALLER.
macro_rules! session {
    ($request:expr, $query:expr, $role:expr) => {
        match session($request, $query, $role) {
            Ok(v) => v,
            Err(e) => return e,
        }
    };
}

/// The topic every transition on `project`'s queue publishes to. One per project, so a consumer polls
/// exactly that log instead of filtering one firehose.
fn goal_topic(project: &str) -> String {
    format!("goal.{project}")
}

const PROJECTS: &str = "projects";
const GOALS: &str = "goals";

/// The goal lifecycle, as the only legal transitions. Nothing may leave `failed` (a requeue makes a NEW
/// goal, so what was tried stays visible) and nothing may reach `done` without having run.
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

/// A project name that is safe as part of a store name and a branch name.
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
    // A sub-goal's parent is checked HERE, where the answer is a 422 naming the problem, rather than leaving a
    // worklist with a child under a parent in another project, or a chain that never ends.
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
        // Empty when this is a goal in its own right. A field rather than a table: a sub-goal IS a goal (same
        // lifecycle, same queue), and a separate table would double every worklist query.
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

/// How deep a decomposition may go. A bound rather than a promise that cycles cannot happen: the walk
/// catches an actual cycle, this catches a chain that is technically a tree and never gets its leaves run.
const MAX_GOAL_DEPTH: usize = 8;

/// May this goal be a parent, for a child in `project`?
///
/// Three refusals, all 422 (fixable by the caller):
///
///   * no such goal — usually an id from another environment;
///   * a different project — a worklist is per project;
///   * too deep, or a cycle — walked rather than assumed, since a chain closing on itself would hang every reader.
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
    // Walk up. `seen` turns an infinite loop into a message: a record written before this check, or edited around it, can still close a cycle.
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
    // `seen` is the PARENT's own chain, so a child hung off it sits one deeper. Checked after the walk, since the depth is not known until then.
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

/// `GET /api/projects/{project}/events?group=<name>&max=<n>`
///
/// The tier-3 observer: a consumer group polling this SEES transitions as they happen instead of diffing
/// `goals_list` snapshots, without being a second place state lives (every event was also a real write on
/// the goal record; a caller that lost them all still gets the right answer from `goals_list`, later).
///
/// `group` defaults to the caller's own subject, so two scripts polling with no group do not silently
/// share (and steal events from) one offset.
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

/// `POST /api/projects/{project}/events/ack {group, ids}`
///
/// Advances `group`'s offset so a later poll does not return the same transitions. Never needed for correctness.
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

/// Move a goal, refusing anything the lifecycle does not allow.
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
    // A goal with live parts may not be finished or thrown away out from under them: their rows would read as
    // already handled, or as work for a goal that no longer exists. Refused rather than cascaded: a destructive
    // multi-record operation should not be inferred from one click.
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
            // Optional: with it, something records which pull request a human should look at and can later watch its status.
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

    // Guarded on the revision we READ, so two people starting the same goal at once cannot both win and
    // leave two runs believing they own one goal.
    match records::update(GOALS, id, &doc.to_string(), entry.revision) {
        Err(records::StoreError::RevisionConflict(_)) => {
            Outcome::Err(409, format!("`{id}` moved while you were looking at it — read it again"))
        }
        Ok(_) => {
            // A lossy hint, never the answer (ADR-0081): a dropped event costs a watcher latency, not correctness, so
            // errors are swallowed; a full or unreachable bus must not make a goal unable to move.
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
