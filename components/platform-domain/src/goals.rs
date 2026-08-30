//! Projects and their queues — ADR-0082's half of the platform.
//!
//! Split out of `lib.rs` because it is the one part of this component that
//! answers a different question from the rest of it. Everything else here is
//! about what is DEPLOYED — components, manifests, environments, the fleet's
//! own status. This is about what is TO BE DONE, and the two only meet at a
//! goal that eventually opens a pull request.
//!
//! The lifecycle is a table rather than scattered `if state == …` checks,
//! because the illegal moves are the interesting ones: nothing leaves `failed`
//! (a requeue makes a NEW goal, so what was tried stays visible), and nothing
//! reaches `done` without having run.

use serde_json::{json, Map, Value};

use crate::bindings::wasi::http::types::IncomingRequest;
use crate::req;
use crate::{caller, now, orgs, personal_org, read_body, records, str_of, Outcome};

// ---- projects and goals (ADR-0082) -----------------------------------------

const PROJECTS: &str = "projects";
const GOALS: &str = "goals";

/// The goal lifecycle, as the only legal transitions.
///
/// A table rather than scattered `if state == …` checks, because the illegal
/// moves are the interesting ones: nothing may leave `failed` (a requeue makes a
/// NEW goal, so what was tried stays visible), and nothing may reach `done`
/// without having run.
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

pub fn project_create(request: &IncomingRequest, query: &Map<String, Value>) -> Outcome {
    let Some(p) = caller(request) else {
        return Outcome::Err(401, "no session".into());
    };
    let org = match orgs::acting(&p.subject, &personal_org(&p), query, orgs::Role::Member) {
        Ok((org, _)) => org,
        Err((code, msg)) => return Outcome::Err(code, msg),
    };
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
    // `owner/name`, checked here rather than at the first forge call, where the
    // answer is a 404 that reads like "the repository does not exist".
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
        // One at a time, which is the whole answer to concurrent pull requests
        // (ADR-0082). Raising it is what makes that a problem worth solving.
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

pub fn projects_list(request: &IncomingRequest, query: &Map<String, Value>) -> Outcome {
    let Some(p) = caller(request) else {
        return Outcome::Err(401, "no session".into());
    };
    let org = match orgs::acting(&p.subject, &personal_org(&p), query, orgs::Role::Viewer) {
        Ok((org, _)) => org,
        Err((code, msg)) => return Outcome::Err(code, msg),
    };
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

pub fn goal_create(request: &IncomingRequest, project: &str, query: &Map<String, Value>) -> Outcome {
    let Some(p) = caller(request) else {
        return Outcome::Err(401, "no session".into());
    };
    let org = match orgs::acting(&p.subject, &personal_org(&p), query, orgs::Role::Member) {
        Ok((org, _)) => org,
        Err((code, msg)) => return Outcome::Err(code, msg),
    };
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
    // A sub-goal's parent is checked HERE, where the answer is a 422 naming the
    // problem. Stored unchecked, the first thing to notice would be a worklist
    // with a child under a parent in another project, or a chain that never ends.
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
        // Empty when this is a goal in its own right, which is most of them. A
        // field rather than a separate table: a sub-goal IS a goal — same
        // lifecycle, same queue, same "a human starts it" — and giving it its own
        // table would mean two of every query that reads a worklist.
        "parent": parent,
        // Queued, and it stays there. A human starts every goal (ADR-0082): there
        // is no loop that drains this, on purpose.
        "state": "queued",
        "created": now(),
    });
    match records::create(GOALS, &doc.to_string(), &["project".to_string(), "org".to_string()]) {
        Ok(e) => Outcome::Json(
            201,
            json!({ "id": e.id, "project": project, "state": "queued", "title": doc["title"] })
                .to_string(),
        ),
        Err(e) => Outcome::Err(500, format!("recording the goal: {e:?}")),
    }
}

/// How deep a decomposition may go.
///
/// A bound rather than a promise that cycles cannot happen: the walk below
/// catches an actual cycle, and this catches the chain that is technically a tree
/// and still means nobody will ever run the leaves.
const MAX_GOAL_DEPTH: usize = 8;

/// May this goal be a parent, for a child in `project`?
///
/// Three refusals, all of them 422 because each is a mistake in the request that
/// the caller can fix:
///
///   * no such goal — usually an id from another environment;
///   * a different project — a worklist is per project, and a child under a
///     parent nobody in that project can see is a row that reads as corrupt;
///   * too deep, or a cycle — walked rather than assumed, because the id is the
///     caller's and a chain that closes on itself would hang every reader of it.
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
    // Walk up. `seen` is what turns an infinite loop into a message: a record
    // written before this check existed, or edited around it, can still close a
    // cycle, and the reader must not be the thing that discovers it.
    let mut seen = vec![parent.to_string()];
    let mut at = str_of(&doc, "parent");
    while !at.is_empty() {
        if seen.contains(&at) {
            return Err((422, format!("goal `{parent}` is already part of a cycle through `{at}`")));
        }
        seen.push(at.clone());
        // A runaway walk is bounded here as well as by the check below, because
        // the two protect different things: this stops the LOOP, that stops the
        // GOAL. A chain longer than the bound is refused either way.
        if seen.len() > MAX_GOAL_DEPTH {
            break;
        }
        let Ok(up) = records::get(GOALS, &at) else { break };
        let Ok(updoc) = serde_json::from_str::<Value>(&up.data) else { break };
        at = str_of(&updoc, "parent");
    }
    // `seen` is the PARENT's own chain, so a child hung off it sits one deeper.
    // Checked after the walk rather than during it: the question is how deep the
    // NEW goal would be, and that is not known until the walk is done.
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

pub fn goals_list(request: &IncomingRequest, project: &str, query: &Map<String, Value>) -> Outcome {
    let Some(p) = caller(request) else {
        return Outcome::Err(401, "no session".into());
    };
    if let Err((code, msg)) = orgs::acting(&p.subject, &personal_org(&p), query, orgs::Role::Viewer)
    {
        return Outcome::Err(code, msg);
    }
    let want = query.get("state").and_then(|v| v.as_str()).unwrap_or_default();
    // `?parent=<id>` lists one goal's parts; `?parent=` (empty, explicitly given)
    // lists only goals that are nobody's part, which is the top-level worklist.
    // Absent means every goal, which is what every existing caller asks for.
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

/// Move a goal, refusing anything the lifecycle does not allow.
pub fn goal_transition(
    request: &IncomingRequest,
    id: &str,
    to: &str,
    query: &Map<String, Value>,
) -> Outcome {
    let Some(p) = caller(request) else {
        return Outcome::Err(401, "no session".into());
    };
    if let Err((code, msg)) = orgs::acting(&p.subject, &personal_org(&p), query, orgs::Role::Member)
    {
        return Outcome::Err(code, msg);
    }
    let Ok(entry) = records::get(GOALS, id) else {
        return Outcome::Err(404, format!("no goal `{id}`"));
    };
    let Ok(mut doc) = serde_json::from_str::<Value>(&entry.data) else {
        return Outcome::Err(500, "the goal record is unreadable".into());
    };
    // A goal with live parts may not be finished or thrown away out from under
    // them. Both leave rows nobody will ever look at again: parts of a goal that
    // is done read as already handled, and parts of an abandoned one read as
    // work still to do for a goal that no longer exists.
    //
    // Refused rather than cascaded. Cascading is a destructive multi-record
    // operation inferred from one click, and the caller has more information than
    // this function does about whether the parts are worth keeping.
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
        // Naming both ends beats "invalid transition": the caller usually has the
        // wrong idea about where the goal currently IS.
        return Outcome::Err(409, format!("a goal cannot go from `{from}` to `{to}`"));
    }

    doc["state"] = json!(to);
    match to {
        "running" => {
            doc["started"] = json!(now());
            // A goal is FROZEN once it starts (ADR-0081): the spec it was judged
            // against must not change under a run. Editing a running goal forks
            // it into a new one instead.
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
        "done" => doc["finished"] = json!(now()),
        _ => {}
    }

    // Guarded on the revision we READ, so two people starting the same goal at the
    // same moment cannot both win. Without it the second write silently overwrites
    // the first and two runs believe they own one goal — which, with one run per
    // project, is exactly the case this design exists to prevent.
    match records::update(GOALS, id, &doc.to_string(), entry.revision) {
        Err(records::StoreError::RevisionConflict(_)) => {
            Outcome::Err(409, format!("`{id}` moved while you were looking at it — read it again"))
        }
        Ok(_) => Outcome::Json(
            200,
            json!({ "id": id, "from": from, "state": to, "title": doc["title"] }).to_string(),
        ),
        Err(e) => Outcome::Err(500, format!("moving the goal: {e:?}")),
    }
}