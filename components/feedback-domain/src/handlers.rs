//! A public feedback board: posts move `open -> planned -> in-progress ->
//! done`, status changes are `admin`-only (a role check, not a row check),
//! and any member may vote once per post. Deleting a post is scoped to its
//! author or an admin — the row-level rule `auth:identity/rbac` cannot
//! express, enforced with `policy:guard` exactly as `crm-domain` enforces
//! "a rep only acts on their own deals".

use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use crate::{audit, introspect, is_admin, Reply, Route};
use serde_json::{json, Value};

const STATUSES: &[&str] = &["open", "planned", "in-progress", "done"];
const POLICY_DOMAIN: &str = "posts";

guestauth::guest_owner_or_admin_policy!(POLICY_DOMAIN, "author");
guestauth::guest_entries_json!();

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "posts"]) => create_post(route, body),
        (Method::Get, ["api", "posts"]) => list_posts(route),
        (Method::Post, ["api", "posts", id, "vote"]) => vote(route, id),
        (Method::Post, ["api", "posts", id, "status"]) => set_status(route, id, body),
        (Method::Delete, ["api", "posts", id]) => delete_post(route, id),
        _ => Reply::err(404, "not_found"),
    }
}

#[derive(serde::Deserialize)]
struct PostReq {
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
}

fn create_post(route: &Route, body: &str) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    let req = guestauth::guest_parse_body!(body, PostReq);
    if req.title.is_empty() {
        return Reply::err(400, "title is required");
    }
    let data = json!({
        "title": req.title,
        "description": req.description,
        "status": "open",
        "votes": 0,
        "voters": [],
        "author": principal.subject,
    })
    .to_string();
    match records::create("posts", &data, &["author".to_string()]) {
        Ok(entry) => {
            audit("post.create", "allow", &principal.subject, &entry.id);
            Reply::json(201, json!({"id": entry.id}))
        }
        Err(_) => Reply::err(500, "store_error"),
    }
}

fn list_posts(route: &Route) -> Reply {
    if introspect(route).is_err() {
        return Reply::err(401, "unauthorized");
    }
    match records::list_records("posts", 100, "") {
        Ok(page) => {
            let mut posts = entries_json(&page.entries);
            posts.sort_by(|a, b| {
                let va = a.get("votes").and_then(Value::as_i64).unwrap_or(0);
                let vb = b.get("votes").and_then(Value::as_i64).unwrap_or(0);
                vb.cmp(&va)
            });
            Reply::json(200, json!({"posts": posts}))
        }
        Err(_) => Reply::err(500, "store_error"),
    }
}

fn vote(route: &Route, id: &str) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    let entry = guestauth::guest_get_or_404!("posts", id);
    let mut post: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));
    let already_voted = post["voters"]
        .as_array()
        .map(|a| a.iter().any(|v| v.as_str() == Some(principal.subject.as_str())))
        .unwrap_or(false);
    if already_voted {
        return Reply::err(409, "already_voted");
    }
    let votes = post.get("votes").and_then(Value::as_i64).unwrap_or(0) + 1;
    post["votes"] = json!(votes);
    match post["voters"].as_array_mut() {
        Some(a) => a.push(json!(principal.subject)),
        None => post["voters"] = json!([principal.subject]),
    }
    match records::update("posts", id, &post.to_string(), entry.revision) {
        Ok(_) => {
            audit("post.vote", "allow", &principal.subject, id);
            Reply::json(200, json!({"id": id, "votes": votes}))
        }
        Err(_) => Reply::err(409, "conflict"),
    }
}

/// Admin-only, checked directly against the role — a status change speaks
/// for the whole team, not for one row's owner.
fn set_status(route: &Route, id: &str, body: &str) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    guestauth::guest_deny_unless!(is_admin(&principal), principal, "post.status", id);
    let entry = guestauth::guest_get_or_404!("posts", id);
    let req: Value = serde_json::from_str(body).unwrap_or(json!({}));
    let status = req.get("status").and_then(Value::as_str).unwrap_or("");
    if !STATUSES.contains(&status) {
        return Reply::err(400, "invalid status");
    }
    let mut post: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));
    post["status"] = json!(status);
    match records::update("posts", id, &post.to_string(), entry.revision) {
        Ok(_) => {
            audit("post.status", "allow", &principal.subject, &format!("{id} -> {status}"));
            Reply::json(200, json!({"id": id, "status": status}))
        }
        Err(_) => Reply::err(409, "conflict"),
    }
}

fn delete_post(route: &Route, id: &str) -> Reply {
    let principal = guestauth::guest_authenticated!(route);
    let entry = guestauth::guest_get_or_404!("posts", id);
    let post: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));
    let author = post.get("author").and_then(Value::as_str).unwrap_or("").to_string();
    guestauth::guest_deny_unless!(
        owns_or_admin("delete", &principal, &author),
        principal,
        "post.delete",
        id
    );
    match records::delete("posts", id) {
        Ok(()) => {
            audit("post.delete", "allow", &principal.subject, id);
            Reply::json(204, Value::Null)
        }
        Err(_) => Reply::err(500, "store_error"),
    }
}
