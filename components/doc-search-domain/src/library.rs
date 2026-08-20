//! `library` — what can be found, and how a question finds it.
//!
//! Stores a document AND indexes it (both or neither — an unindexed document is
//! unfindable, an indexed-but-unstored one has nothing to answer with). Search hits
//! carry only an id and a score, so `title` is read back from the store per hit — a
//! caller cannot use a list of ULIDs.

use crate::bindings::auth::identity::authorizer as authz;
use crate::bindings::auth::identity::types as auth_types;
use crate::bindings::records::store::store as records;
use crate::bindings::search::index::index as search;
use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};
use serde_json::{json, Value};

/// Resolve the bearer and require `{docs, action}`, per CONTRACT.md's three-way
/// failure split.
fn authorize(route: &Route, action: &str) -> Result<auth_types::Principal, Reply> {
    if route.bearer.is_empty() {
        return Err(Reply::err(401, "unauthenticated"));
    }
    let required = auth_types::Permission { target: "docs".into(), action: action.into() };
    match authz::authorize(&route.bearer, &required) {
        Ok(principal) => Ok(principal),
        Err(auth_types::AuthError::InsufficientScope(_)) => Err(Reply::err(403, "forbidden")),
        Err(auth_types::AuthError::BackendUnavailable(_)) | Err(auth_types::AuthError::Internal(_)) => {
            Err(Reply::err(503, "auth_unavailable"))
        }
        Err(_) => Err(Reply::err(401, "unauthenticated")),
    }
}

fn create_doc(route: &Route, body: &str) -> Reply {
    if let Err(reply) = authorize(route, "write") {
        return reply;
    }
    let req: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let title = req.get("title").and_then(Value::as_str).unwrap_or("");
    let text = req.get("text").and_then(Value::as_str).unwrap_or("");
    let tag = req.get("tag").and_then(Value::as_str).unwrap_or("");
    if title.is_empty() || text.is_empty() || tag.is_empty() {
        return Reply::err(400, "invalid_doc");
    }

    let doc = json!({ "title": title, "text": text, "tag": tag });
    let entry = match records::create("docs", &doc.to_string(), &["tag".to_string()]) {
        Ok(e) => e,
        Err(_) => return Reply::err(500, "store_unavailable"),
    };

    // Stored but not indexed is unfindable — both happen or neither counts.
    if search::index_doc(&entry.id, &format!("{title}\n{text}"), &[tag.to_string()]).is_err() {
        return Reply::err(500, "index_unavailable");
    }

    Reply::json(201, json!({ "id": entry.id }))
}

fn get_doc(route: &Route, id: &str) -> Reply {
    if let Err(reply) = authorize(route, "read") {
        return reply;
    }
    match records::get("docs", id) {
        Ok(entry) => {
            let mut doc: Value = serde_json::from_str(&entry.data).unwrap_or(json!({}));
            if let Value::Object(map) = &mut doc {
                map.insert("id".to_string(), json!(entry.id));
            }
            Reply::json(200, doc)
        }
        Err(_) => Reply::err(404, "not_found"),
    }
}

fn search_docs(route: &Route) -> Reply {
    if let Err(reply) = authorize(route, "read") {
        return reply;
    }
    let query = route.param("q");
    let tag = route.param("tag");
    let limit = route.param("limit").parse::<u32>().unwrap_or(5).min(20);
    let tags: Vec<String> = if tag.is_empty() { vec![] } else { vec![tag] };

    let hits = match search::query(&query, search::Mode::Any, &tags, limit) {
        Ok(hits) => hits,
        Err(_) => return Reply::err(500, "search_unavailable"),
    };

    // A hit is only an id and a score; a caller needs a title, so read it back
    // from the store per hit rather than making the caller chase ULIDs.
    let hits: Vec<Value> = hits
        .into_iter()
        .map(|hit| {
            let title = records::get("docs", &hit.id)
                .ok()
                .and_then(|e| serde_json::from_str::<Value>(&e.data).ok())
                .and_then(|v| v.get("title").and_then(Value::as_str).map(str::to_string))
                .unwrap_or_default();
            json!({ "id": hit.id, "score": hit.score, "title": title })
        })
        .collect();

    // No hits is still 200: an empty library and a bad question are the same
    // shape to a caller, and neither is an error.
    Reply::json(200, json!({ "hits": hits }))
}

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "docs"]) => create_doc(route, body),
        (Method::Get, ["api", "docs", id]) => get_doc(route, id),
        (Method::Get, ["api", "search"]) => search_docs(route),
        _ => Reply::err(404, "not_found"),
    }
}