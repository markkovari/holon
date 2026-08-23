//! Staff accounts and pet search — the `access-and-search` part.
//!
//! Nothing here hashes, tokenises or scans. `auth:identity/accounts` owns the
//! password, `auth:identity/session` owns the token, `search:index/index` owns
//! the ranking. This file is glue.
//!
//! Login is deliberately NOT `accounts::login` (which would hand back a pair we
//! never asked for): `verify-password` proves the credential, `session::issue`
//! mints the token we then `lookup` on every search. Same two calls guard both
//! directions, so a token this component accepts is one this component issued.
//!
//! The corpus comes from the record store, synced on search — `owners-and-pets`
//! is not ours to edit and needs to know nothing about the index.

use crate::bindings::auth::identity::accounts;
use crate::bindings::auth::identity::session;
use crate::bindings::auth::identity::types::AuthError;
use crate::bindings::records::store::store as records;
use crate::bindings::search::index::index as search;
use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};
use serde_json::{json, Value};

const TENANT: &str = "clinic";
const PETS: &str = "pets";
const PET_TAG: &str = "kind:pet";

pub fn handle(method: &Method, route: &Route, body: &str) -> Reply {
    let seg: Vec<&str> = route.segments.iter().map(String::as_str).collect();
    match (method, seg.as_slice()) {
        (Method::Post, ["api", "staff"]) => register(body),
        (Method::Post, ["api", "staff", "login"]) => login(body),
        (Method::Get, ["api", "pets", "search"]) => search_pets(route),
        _ => Reply::err(404, "not_found"),
    }
}

/// `{email, password}` out of a body, or the 400 to answer with.
fn creds(body: &str) -> Result<(String, String), Reply> {
    let doc: Value = serde_json::from_str(body).map_err(|_| Reply::err(400, "invalid"))?;
    let email = doc["email"].as_str().unwrap_or("").trim().to_string();
    let password = doc["password"].as_str().unwrap_or("").to_string();
    Ok((email, password))
}

fn register(body: &str) -> Reply {
    let (email, password) = match creds(body) {
        Ok(c) => c,
        Err(r) => return r,
    };
    // Checked here as well as in auth-guard: the contract promises 400 for a
    // short password regardless of how `password-min-len` is configured.
    if !email.contains('@') || password.chars().count() < 8 {
        return Reply::err(400, "invalid");
    }
    match accounts::register(&email, &password, TENANT) {
        Ok(p) => Reply::json(201, json!({ "id": p.subject, "email": email })),
        Err(AuthError::AlreadyExists) => Reply::err(409, "taken"),
        Err(AuthError::Malformed(_)) => Reply::err(400, "invalid"),
        Err(AuthError::RateLimited(_)) => Reply::err(429, "rate_limited"),
        Err(AuthError::BackendUnavailable(_)) => Reply::err(503, "unavailable"),
        Err(_) => Reply::err(500, "internal"),
    }
}

fn login(body: &str) -> Reply {
    let (email, password) = match creds(body) {
        Ok(c) => c,
        Err(r) => return r,
    };
    // One answer for "no such account" and "wrong password": no enumeration.
    let Ok(principal) = accounts::verify_password(&email, &password, TENANT) else {
        return Reply::err(401, "unauthorized");
    };
    match session::issue(&principal) {
        Ok(pair) => Reply::json(200, json!({ "token": pair.access_token })),
        Err(_) => Reply::err(503, "unavailable"),
    }
}

/// Bring the index level with the store.
///
/// ponytail: the guard is a document count, so a same-sized store that changed
/// (one pet deleted, one added) is missed until the count moves again. The fix
/// is indexing on create — that is `src/owners.rs`, which this part may not
/// edit; ask via CONTRACT-REQUEST.md if the drift ever matters.
fn sync_index() {
    let Ok(page) = records::list_records(PETS, 1000, "") else { return };
    if search::doc_count().unwrap_or(0) as usize == page.entries.len() {
        return;
    }
    let tags = [PET_TAG.to_string()];
    for entry in &page.entries {
        let Ok(doc) = serde_json::from_str::<Value>(&entry.data) else { continue };
        let text = format!(
            "{} {}",
            doc["name"].as_str().unwrap_or(""),
            doc["species"].as_str().unwrap_or("")
        );
        let _ = search::index_doc(&entry.id, &text, &tags);
    }
}

fn search_pets(route: &Route) -> Reply {
    if route.bearer.is_empty() || session::lookup(&route.bearer).is_err() {
        return Reply::err(401, "unauthorized");
    }
    let q = route.param("q").trim().to_string();
    if q.is_empty() {
        return Reply::err(400, "invalid");
    }
    sync_index();
    // ANY, not ALL: "cat marbles" should still rank Marbles first rather than
    // return nothing. Ranking is the index's job, ordering here is its output.
    let hits = match search::query(&q, search::Mode::Any, &[PET_TAG.to_string()], 50) {
        Ok(h) => h,
        Err(_) => return Reply::err(503, "unavailable"),
    };
    let pets: Vec<Value> = hits
        .iter()
        .filter_map(|hit| {
            let entry = records::get(PETS, &hit.id).ok()?;
            let doc: Value = serde_json::from_str(&entry.data).ok()?;
            Some(json!({
                "id": hit.id,
                "name": doc["name"],
                "species": doc["species"],
                "owner_id": doc["owner_id"],
            }))
        })
        .collect();
    Reply::json(200, json!({ "pets": pets }))
}
