use crate::bindings::auth::identity::authorizer as authz;
use crate::bindings::auth::identity::types::Permission;
use crate::bindings::records::store::store as records;
use crate::bindings::search::index::index as search;
use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};
use serde_json::{json, Value};

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "docs"]) => create_doc(route, body),
        (Method::Get, ["api", "docs", id]) => get_doc(route, id),
        (Method::Get, ["api", "search"]) => search_docs(route),
        _ => Reply::err(404, "not_found"),
    }
}

fn authorize_perm(route: &Route, action: &str) -> Result<String, Reply> {
    let perm = Permission { target: "docs".to_string(), action: action.to_string() };
    match authz::authorize(&route.bearer, &perm) {
        Ok(p) => Ok(p.subject),
        Err(err) => {
            use crate::bindings::auth::identity::types::AuthError;
            let reply = match err {
                AuthError::InsufficientScope(_) => Reply::err(403, "forbidden"),
                AuthError::BackendUnavailable(_) | AuthError::Internal(_) => {
                    Reply::err(503, "auth_unavailable")
                }
                _ => Reply::err(401, "unauthenticated"),
            };
            Err(reply)
        }
    }
}

fn create_doc(route: &Route, body: &str) -> Reply {
    if let Err(r) = authorize_perm(route, "write") {
        return r;
    }
    let req: Value = serde_json::from_str(body).unwrap_or(json!({}));
    let title = req.get("title").and_then(Value::as_str).unwrap_or("");
    let text = req.get("text").and_then(Value::as_str).unwrap_or("");
    let tag = req.get("tag").and_then(Value::as_str).unwrap_or("");

    if title.is_empty() || text.is_empty() || tag.is_empty() {
        return Reply::err(400, "invalid_doc");
    }

    let doc_str = json!({
        "title": title,
        "text": text,
        "tag": tag
    })
    .to_string();

    let entry = match records::create("docs", &doc_str, &["tag".to_string()]) {
        Ok(e) => e,
        Err(_) => return Reply::err(500, "store_error"),
    };

    let index_text = format!("{}\n{}", title, text);
    if search::index_doc(&entry.id, &index_text, &[tag.to_string()]).is_err() {
        return Reply::err(500, "index_error");
    }

    Reply::json(201, json!({"id": entry.id}))
}

fn get_doc(route: &Route, id: &str) -> Reply {
    if let Err(r) = authorize_perm(route, "read") {
        return r;
    }
    match records::get("docs", id) {
        Ok(e) => {
            let mut v: Value = serde_json::from_str(&e.data).unwrap_or(json!({}));
            if let Value::Object(ref mut m) = v {
                m.insert("id".to_string(), json!(e.id));
            }
            Reply::json(200, v)
        }
        Err(_) => Reply::err(404, "not_found"),
    }
}

fn search_docs(route: &Route) -> Reply {
    if let Err(r) = authorize_perm(route, "read") {
        return r;
    }
    let q = route.param("q");
    let tag = route.param("tag");
    let limit_str = route.param("limit");
    let limit = limit_str.parse::<u32>().unwrap_or(5).min(20);

    let tags = if tag.is_empty() { vec![] } else { vec![tag.clone()] };

    let hits = match search::query(&q, search::Mode::Any, &tags, limit) {
        Ok(h) => h,
        Err(_) => return Reply::err(500, "search_error"), // search error
    };

    let mut results = Vec::new();
    for hit in hits {
        let title = match records::get("docs", &hit.id) {
            Ok(e) => {
                let v: Value = serde_json::from_str(&e.data).unwrap_or(json!({}));
                v.get("title").and_then(Value::as_str).unwrap_or("").to_string()
            }
            Err(_) => "".to_string(),
        };
        results.push(json!({
            "id": hit.id,
            "score": hit.score,
            "title": title
        }));
    }

    Reply::json(200, json!({"hits": results}))
}
